//! Runtime-owned topic and initializer arenas.
//!
//! Published topic and single-flight initializer allocations are retained as
//! tombstones until the handle service is reclaimed. Read-side maps publish
//! copyable pointers only; they never share ownership or run reclamation.

#![allow(
    unsafe_code,
    reason = "topic publication uses stable non-owning pointers into table-owned arenas"
)]
#![allow(
    clippy::vec_box,
    reason = "published topics must keep their stable heap address while deferred reclamation is pending"
)]

#[cfg(any(target_os = "windows", test))]
use super::FormulaLifetimeGeneration;
use super::{FormulaObserverId, HandleTopicKey, Topic};
use crate::generation::TopicGeneration;
use crate::{XllError, XllResult};
use parking_lot::{Condvar, Mutex, RwLock};
#[cfg(test)]
use parking_lot::{RwLockReadGuard, RwLockWriteGuard};
use rustc_hash::{FxHashMap, FxHasher};
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::ops::Deref;
use std::ptr::NonNull;
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::thread::ThreadId;
use xlfn_kernel::drain_gate::DEFAULT_STRIPE_COUNT;
use xlfn_kernel::rotating_read_domain::{
    DrainedGeneration, RotatingReadDomain, RotatingReadPermit,
};

const MIN_PUBLISHED_TOPIC_SHARDS: usize = 64;
const TARGET_TOPICS_PER_SHARD: usize = 64;

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PublishedTopicState {
    Provisional = 0,
    Live = 1,
    Stale = 2,
    Closing = 3,
}

impl PublishedTopicState {
    fn from_raw(raw: u8) -> Self {
        match raw {
            value if value == Self::Provisional as u8 => Self::Provisional,
            value if value == Self::Live as u8 => Self::Live,
            value if value == Self::Stale as u8 => Self::Stale,
            value if value == Self::Closing as u8 => Self::Closing,
            _ => Self::Stale,
        }
    }
}

pub(crate) struct PublishedTopic {
    pub(crate) token: String,
    pub(crate) lifetime_key: String,
    pub(crate) state: AtomicU8,
}

impl PublishedTopic {
    pub(crate) fn new(token: String, lifetime_key: String) -> Self {
        Self {
            token,
            lifetime_key,
            state: AtomicU8::new(PublishedTopicState::Provisional as u8),
        }
    }

    pub(crate) fn state(&self) -> PublishedTopicState {
        PublishedTopicState::from_raw(self.state.load(Ordering::Acquire))
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) struct PublishedTopicPtr(pub(crate) NonNull<PublishedTopic>);

impl PublishedTopicPtr {
    pub(crate) fn from_ref(topic: &PublishedTopic) -> Self {
        Self(NonNull::from(topic))
    }
}

// SAFETY: PublishedTopicPtr is an audited pointer to an immutable PublishedTopic.
unsafe impl Send for PublishedTopicPtr {}
// SAFETY: PublishedTopic is thread-safe and immutable borrows can be shared.
unsafe impl Sync for PublishedTopicPtr {}

/// A scoped read capability that protects published topics from being reclaimed
/// while they are being inspected.
pub(crate) struct TopicReadLease<'a> {
    published: &'a PublishedTopics,
    _permit: RotatingReadPermit<'a, DEFAULT_STRIPE_COUNT>,
    _guard: super::runtime::TopicReadGuard,
}

impl<'a> TopicReadLease<'a> {
    pub(crate) fn load(&self, key: &HandleTopicKey) -> Option<PublishedTopicRef<'_>> {
        let ptr = self.published.load(key)?;
        Some(PublishedTopicRef {
            ptr: ptr.0,
            _marker: PhantomData,
        })
    }
}

/// A borrowed reference to a `PublishedTopic` whose lifetime is tied to an active
/// `TopicReadLease`.
pub(crate) struct PublishedTopicRef<'a> {
    ptr: NonNull<PublishedTopic>,
    _marker: PhantomData<&'a PublishedTopic>,
}

impl<'a> Deref for PublishedTopicRef<'a> {
    type Target = PublishedTopic;

    fn deref(&self) -> &Self::Target {
        // SAFETY: PublishedTopicRef is acquired from a live TopicReadLease,
        // which holds an active read-domain permit. The read domain guarantees
        // that PublishedTopic cannot be reclaimed until after the permit is dropped.
        unsafe { self.ptr.as_ref() }
    }
}

pub(crate) struct PublishedTopics {
    shards: Box<[RwLock<FxHashMap<HandleTopicKey, PublishedTopicPtr>>]>,
    shard_mask: usize,
}

impl PublishedTopics {
    pub(crate) fn new(maximum_bindings: usize) -> Self {
        let shard_count = shard_count_for(maximum_bindings);
        Self {
            shards: (0..shard_count)
                .map(|_| RwLock::new(FxHashMap::default()))
                .collect(),
            shard_mask: shard_count - 1,
        }
    }

    fn shard_index(&self, key: &HandleTopicKey) -> usize {
        let mut hasher = FxHasher::default();
        key.hash(&mut hasher);
        (hasher.finish() as usize) & self.shard_mask
    }

    pub(crate) fn load(&self, key: &HandleTopicKey) -> Option<PublishedTopicPtr> {
        self.shards[self.shard_index(key)].read().get(key).copied()
    }

    fn insert(&self, key: HandleTopicKey, topic: PublishedTopicPtr) {
        if self.shards[self.shard_index(&key)]
            .write()
            .insert(key, topic)
            .is_some()
        {
            xlfn_kernel::invariant::fail_stop();
        }
    }

    fn remove(&self, key: HandleTopicKey) {
        self.shards[self.shard_index(&key)].write().remove(&key);
    }

    fn clear(&self) {
        for shard in &self.shards {
            shard.write().clear();
        }
    }
}

fn shard_count_for(maximum_bindings: usize) -> usize {
    let required = maximum_bindings.max(1).div_ceil(TARGET_TOPICS_PER_SHARD);
    required.next_power_of_two().max(MIN_PUBLISHED_TOPIC_SHARDS)
}

#[derive(Clone)]
pub(crate) struct InitializationPtr(Arc<Initialization>);

impl InitializationPtr {
    pub(crate) fn new(initialization: Initialization) -> Self {
        Self(Arc::new(initialization))
    }
}

impl Deref for InitializationPtr {
    type Target = Initialization;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl PartialEq for InitializationPtr {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for InitializationPtr {}

pub(crate) struct TopicTableState {
    pub(crate) by_key: FxHashMap<HandleTopicKey, Topic>,
    pub(crate) by_lifetime_key: FxHashMap<String, HandleTopicKey>,
    pub(crate) by_observer_id: FxHashMap<FormulaObserverId, HandleTopicKey>,
    pub(crate) initializing: FxHashMap<HandleTopicKey, InitializationPtr>,
    pub(crate) generation: TopicGeneration,
    pub(crate) closed: bool,
}

impl Default for TopicTableState {
    fn default() -> Self {
        Self {
            by_key: FxHashMap::default(),
            by_lifetime_key: FxHashMap::default(),
            by_observer_id: FxHashMap::default(),
            initializing: FxHashMap::default(),
            generation: TopicGeneration::ONE,
            closed: false,
        }
    }
}

pub(crate) struct TopicTable {
    state: RwLock<TopicTableState>,
    published: PublishedTopics,
    read_domain: RotatingReadDomain<DEFAULT_STRIPE_COUNT>,
    pending_reclaims: [Mutex<Vec<Box<PublishedTopic>>>; 2],
}

impl TopicTable {
    pub(crate) fn new(maximum_bindings: usize) -> Self {
        Self {
            state: RwLock::new(TopicTableState::default()),
            published: PublishedTopics::new(maximum_bindings),
            read_domain: RotatingReadDomain::new(),
            pending_reclaims: [Mutex::new(Vec::new()), Mutex::new(Vec::new())],
        }
    }

    pub(crate) fn enter_read_lease(&self) -> XllResult<TopicReadLease<'_>> {
        let permit = self
            .read_domain
            .enter_current_thread()
            .map_err(|_| XllError::Closing)?;
        Ok(TopicReadLease {
            published: &self.published,
            _permit: permit,
            _guard: super::runtime::TopicReadGuard::enter(),
        })
    }

    fn enqueue_reclaim(&self, topic: Box<PublishedTopic>) {
        loop {
            let generation = self.read_domain.current_generation();
            let mut queue = self.pending_reclaims[generation.index()].lock();
            if self.read_domain.current_generation() != generation {
                drop(queue);
                std::hint::spin_loop();
                continue;
            }
            queue.push(topic);
            return;
        }
    }

    fn drain_generation(&self, generation: DrainedGeneration) -> Vec<Box<PublishedTopic>> {
        let mut queue = self.pending_reclaims[generation.index()].lock();
        std::mem::take(&mut *queue)
    }

    pub(crate) fn try_quiesce_and_drain(&self) -> Vec<Box<PublishedTopic>> {
        if self.pending_reclaims[0].lock().is_empty() && self.pending_reclaims[1].lock().is_empty()
        {
            return Vec::new();
        }
        let Some(result) = self
            .read_domain
            .try_quiesce_if_idle(|generation| self.drain_generation(generation))
        else {
            return Vec::new();
        };
        result.unwrap_or_default()
    }

    pub(crate) fn seal_and_drain(&self) -> Vec<Box<PublishedTopic>> {
        self.read_domain.seal_and_wait();
        let mut queue0 = self.pending_reclaims[0].lock();
        let mut queue1 = self.pending_reclaims[1].lock();
        let mut all = std::mem::take(&mut *queue0);
        all.append(&mut *queue1);
        all
    }

    #[cfg(test)]
    pub(crate) fn read(&self) -> RwLockReadGuard<'_, TopicTableState> {
        self.state.read()
    }

    #[cfg(test)]
    pub(crate) fn write(&self) -> RwLockWriteGuard<'_, TopicTableState> {
        self.state.write()
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.state.read().closed
    }

    #[cfg(test)]
    pub(crate) fn published(&self) -> &PublishedTopics {
        &self.published
    }

    pub(crate) fn prepare_decision(
        &self,
        key: HandleTopicKey,
        owner: ThreadId,
        make_initialization: impl FnOnce() -> Initialization,
    ) -> XllResult<PrepareDecision> {
        let state = self.state.read();
        if state.closed {
            return Err(XllError::Closing);
        }
        if let Some(initialization) = state.initializing.get(&key).cloned() {
            if initialization.owner == owner {
                return Err(XllError::ReentrantCall);
            }
            return Ok(PrepareDecision::Wait { initialization });
        }
        if let Some(topic) = state.by_key.get(&key) {
            return Ok(PrepareDecision::Existing {
                token: topic.publication.token.clone(),
                lifetime_key: topic.publication.lifetime_key.clone(),
                generation: state.generation,
            });
        }
        drop(state);

        let mut state = self.state.write();
        if state.closed {
            return Err(XllError::Closing);
        }
        if let Some(initialization) = state.initializing.get(&key).cloned() {
            if initialization.owner == owner {
                return Err(XllError::ReentrantCall);
            }
            return Ok(PrepareDecision::Wait { initialization });
        }
        if let Some(topic) = state.by_key.get(&key) {
            return Ok(PrepareDecision::Existing {
                token: topic.publication.token.clone(),
                lifetime_key: topic.publication.lifetime_key.clone(),
                generation: state.generation,
            });
        }

        let initialization = InitializationPtr::new(make_initialization());
        let generation = state.generation;
        state.initializing.insert(key, initialization.clone());
        Ok(PrepareDecision::Initialize {
            initialization,
            generation,
        })
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn claim_lifetime(
        &self,
        lifetime_key: &str,
        lifetime_generation: FormulaLifetimeGeneration,
    ) -> XllResult<HandleTopicKey> {
        let mut state = self.state.write();
        if state.closed {
            return Err(XllError::Closing);
        }
        let key = state
            .by_lifetime_key
            .get(lifetime_key)
            .copied()
            .ok_or(XllError::StaleHandle)?;
        let topic = state.by_key.get_mut(&key).ok_or(XllError::StaleHandle)?;
        if topic
            .lifetime_generation
            .is_some_and(|existing| existing != lifetime_generation)
        {
            return Err(XllError::InvalidHandle);
        }
        topic.lifetime_generation = Some(lifetime_generation);
        Ok(key)
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn connect(
        &self,
        lifetime_generation: FormulaLifetimeGeneration,
        owner: FormulaObserverId,
        lifetime_key: &str,
    ) -> XllResult<(HandleTopicKey, String, bool)> {
        let mut state = self.state.write();
        if state.closed {
            return Err(XllError::Closing);
        }
        let key = state
            .by_lifetime_key
            .get(lifetime_key)
            .copied()
            .ok_or(XllError::StaleHandle)?;
        if state
            .by_observer_id
            .get(&owner)
            .is_some_and(|existing| existing != &key)
        {
            return Err(XllError::InvalidHandle);
        }
        let (token, created) = {
            let topic = state.by_key.get_mut(&key).ok_or(XllError::StaleHandle)?;
            if topic
                .lifetime_generation
                .is_some_and(|existing| existing != lifetime_generation)
            {
                return Err(XllError::InvalidHandle);
            }
            topic.lifetime_generation = Some(lifetime_generation);
            let created = if let Some(existing) = topic.observer {
                if existing != owner {
                    return Err(XllError::InvalidHandle);
                }
                if !topic.observer_committed {
                    return Err(XllError::Overloaded);
                }
                false
            } else {
                topic.observer = Some(owner);
                topic.observer_committed = false;
                true
            };
            (topic.publication.token.clone(), created)
        };
        state.by_observer_id.insert(owner, key);
        Ok((key, token, created))
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn commit_connection(
        &self,
        owner: FormulaObserverId,
        key: HandleTopicKey,
    ) -> XllResult<()> {
        let mut state = self.state.write();
        if state.closed {
            return Err(XllError::Closing);
        }
        if state.by_observer_id.get(&owner) != Some(&key) {
            return Err(XllError::StaleHandle);
        }
        let topic = state.by_key.get_mut(&key).ok_or(XllError::StaleHandle)?;
        if topic.observer != Some(owner) {
            return Err(XllError::StaleHandle);
        }
        topic.observer_committed = true;
        Ok(())
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn rollback_connection(
        &self,
        owner: FormulaObserverId,
        key: HandleTopicKey,
    ) -> bool {
        let mut state = self.state.write();
        if state.by_observer_id.get(&owner) != Some(&key)
            || !state
                .by_key
                .get(&key)
                .is_some_and(|topic| topic.observer == Some(owner) && !topic.observer_committed)
        {
            return false;
        }
        state.by_observer_id.remove(&owner);
        if let Some(topic) = state.by_key.get_mut(&key) {
            topic.observer = None;
            topic.observer_committed = false;
        }
        true
    }

    pub(crate) fn insert_provisional(
        &self,
        key: HandleTopicKey,
        generation: TopicGeneration,
        publication: PublishedTopic,
        on_linearized: impl FnOnce(PublishedTopicPtr),
    ) -> XllResult<PublishedTopicPtr> {
        let mut state = self.state.write();
        if state.closed || state.generation != generation {
            return Err(XllError::Closing);
        }
        if state.by_key.contains_key(&key)
            || state
                .by_lifetime_key
                .contains_key(publication.lifetime_key.as_str())
        {
            return Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::HANDLE_TOPIC_COLLISION,
            });
        }
        let lifetime_key = publication.lifetime_key.clone();
        let publication = Box::new(publication);
        state.by_key.insert(
            key,
            Topic {
                publication,
                #[cfg(any(target_os = "windows", test))]
                lifetime_generation: None,
                observer: None,
                #[cfg(any(target_os = "windows", test))]
                observer_committed: false,
            },
        );
        let pointer =
            PublishedTopicPtr::from_ref(state.by_key.get(&key).unwrap().publication.as_ref());
        state.by_lifetime_key.insert(lifetime_key, key);
        on_linearized(pointer);
        Ok(pointer)
    }

    pub(crate) fn commit_publication(
        &self,
        key: HandleTopicKey,
        generation: TopicGeneration,
        initialization: InitializationPtr,
        publication: PublishedTopicPtr,
        on_linearized: impl FnOnce(),
    ) -> XllResult<()> {
        let mut state = self.state.write();
        if state.closed || state.generation != generation {
            return Err(XllError::Closing);
        }
        if state.initializing.get(&key) != Some(&initialization) {
            return Err(XllError::StaleHandle);
        }
        let Some(topic) = state.by_key.get_mut(&key) else {
            return Err(XllError::StaleHandle);
        };
        if PublishedTopicPtr::from_ref(topic.publication.as_ref()) != publication {
            return Err(XllError::StaleHandle);
        }
        topic
            .publication
            .state
            .store(PublishedTopicState::Live as u8, Ordering::Release);
        self.published.insert(key, publication);
        state.initializing.remove(&key);
        on_linearized();
        Ok(())
    }

    pub(crate) fn finish_initialization(
        &self,
        key: HandleTopicKey,
        initialization: InitializationPtr,
    ) -> bool {
        let mut state = self.state.write();
        if state.initializing.get(&key) == Some(&initialization) {
            state.initializing.remove(&key);
            true
        } else {
            false
        }
    }

    pub(crate) fn is_current(
        &self,
        key: HandleTopicKey,
        generation: TopicGeneration,
        token: &str,
    ) -> XllResult<()> {
        let state = self.state.read();
        if state.closed || state.generation != generation {
            return Err(XllError::Closing);
        }
        if state
            .by_key
            .get(&key)
            .is_some_and(|topic| topic.publication.token == token)
        {
            Ok(())
        } else {
            Err(XllError::StaleHandle)
        }
    }

    fn remove_topic_locked(
        &self,
        state: &mut TopicTableState,
        key: HandleTopicKey,
    ) -> Option<(TopicRemoval, Box<PublishedTopic>)> {
        let topic = state.by_key.get(&key)?;
        let was_provisional = topic.publication.state() == PublishedTopicState::Provisional;
        let initialization_id = state
            .initializing
            .get(&key)
            .map(|initialization| initialization.refinement_id);
        topic
            .publication
            .state
            .store(PublishedTopicState::Stale as u8, Ordering::Release);
        self.published.remove(key);
        let topic = state.by_key.remove(&key)?;
        state
            .by_lifetime_key
            .remove(topic.publication.lifetime_key.as_str());
        if let Some(owner) = topic.observer {
            state.by_observer_id.remove(&owner);
        }
        Some((
            TopicRemoval {
                token: topic.publication.token.clone(),
                key,
                was_provisional,
                initialization_id,
            },
            topic.publication,
        ))
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn remove_by_observer(&self, owner: FormulaObserverId) -> Option<TopicRemoval> {
        let mut state = self.state.write();
        let key = state.by_observer_id.remove(&owner)?;
        let (removal, retired) = self.remove_topic_locked(&mut state, key)?;
        drop(state);
        self.enqueue_reclaim(retired);
        if !super::runtime::is_current_thread_reading_topic() {
            let drained = self.try_quiesce_and_drain();
            drop(drained);
        }
        Some(removal)
    }

    #[cfg(test)]
    pub(crate) fn remove_by_lifetime_key(&self, lifetime_key: &str) -> Option<TopicRemoval> {
        let mut state = self.state.write();
        let key = state.by_lifetime_key.get(lifetime_key).copied()?;
        let (removal, retired) = self.remove_topic_locked(&mut state, key)?;
        drop(state);
        self.enqueue_reclaim(retired);
        if !super::runtime::is_current_thread_reading_topic() {
            let drained = self.try_quiesce_and_drain();
            drop(drained);
        }
        Some(removal)
    }

    pub(crate) fn remove_topic_if_token(
        &self,
        key: HandleTopicKey,
        token: &str,
        on_linearized: impl FnOnce(),
    ) -> Option<TopicRemoval> {
        let mut state = self.state.write();
        if !state
            .by_key
            .get(&key)
            .is_some_and(|topic| topic.publication.token == token)
        {
            return None;
        }
        let (removal, retired) = self.remove_topic_locked(&mut state, key)?;
        drop(state);
        on_linearized();
        self.enqueue_reclaim(retired);
        if !super::runtime::is_current_thread_reading_topic() {
            let drained = self.try_quiesce_and_drain();
            drop(drained);
        }
        Some(removal)
    }

    pub(crate) fn close(&self) -> Vec<InitializationPtr> {
        let mut state = self.state.write();
        state.closed = true;
        state.generation = state.generation.next().unwrap_or(state.generation);
        for (_, topic) in state.by_key.drain() {
            topic
                .publication
                .state
                .store(PublishedTopicState::Closing as u8, Ordering::Release);
            self.enqueue_reclaim(topic.publication);
        }
        self.published.clear();
        state.by_lifetime_key.clear();
        state.by_observer_id.clear();
        state
            .initializing
            .drain()
            .map(|(_, initialization)| initialization)
            .collect()
    }

    pub(crate) fn remove_all(&self) -> Vec<TopicRemoval> {
        let mut state = self.state.write();
        let keys = state.by_key.keys().copied().collect::<Vec<_>>();
        let mut removals = Vec::with_capacity(keys.len());
        let mut retired_topics = Vec::with_capacity(keys.len());
        for key in keys {
            if let Some((removal, retired)) = self.remove_topic_locked(&mut state, key) {
                removals.push(removal);
                retired_topics.push(retired);
            }
        }
        drop(state);
        for retired in retired_topics {
            self.enqueue_reclaim(retired);
        }
        if !super::runtime::is_current_thread_reading_topic() {
            let drained = self.try_quiesce_and_drain();
            drop(drained);
        }
        removals
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn remove_generation(
        &self,
        lifetime_generation: FormulaLifetimeGeneration,
    ) -> Vec<TopicRemoval> {
        let mut state = self.state.write();
        let keys = state
            .by_key
            .iter()
            .filter(|(_, topic)| topic.lifetime_generation == Some(lifetime_generation))
            .map(|(key, _)| *key)
            .collect::<Vec<_>>();
        let mut removals = Vec::with_capacity(keys.len());
        let mut retired_topics = Vec::with_capacity(keys.len());
        for key in keys {
            if let Some((removal, retired)) = self.remove_topic_locked(&mut state, key) {
                removals.push(removal);
                retired_topics.push(retired);
            }
        }
        drop(state);
        for retired in retired_topics {
            self.enqueue_reclaim(retired);
        }
        if !super::runtime::is_current_thread_reading_topic() {
            let drained = self.try_quiesce_and_drain();
            drop(drained);
        }
        removals
    }
}

pub(crate) struct TopicRemoval {
    pub(crate) token: String,
    pub(crate) key: HandleTopicKey,
    pub(crate) was_provisional: bool,
    pub(crate) initialization_id: Option<u64>,
}

pub(crate) struct Initialization {
    pub(crate) owner: ThreadId,
    pub(crate) owner_done: AtomicBool,
    pub(crate) wait: Mutex<()>,
    pub(crate) completed: Condvar,
    pub(crate) refinement_id: u64,
    #[cfg(test)]
    pub(crate) waiters: AtomicUsize,
}

impl Initialization {
    pub(crate) fn wait_until_done(&self) {
        let mut wait = self.wait.lock();
        while !self.owner_done.load(Ordering::Acquire) {
            self.completed.wait(&mut wait);
        }
    }

    pub(crate) fn wait_until_done_or_closed(&self, topics: &TopicTable) {
        #[cfg(test)]
        self.waiters.fetch_add(1, Ordering::AcqRel);
        let mut wait = self.wait.lock();
        while !self.owner_done.load(Ordering::Acquire) && !topics.is_closed() {
            self.completed.wait(&mut wait);
        }
        #[cfg(test)]
        self.waiters.fetch_sub(1, Ordering::AcqRel);
    }

    pub(crate) fn complete(&self) {
        let _wait = self.wait.lock();
        self.owner_done.store(true, Ordering::Release);
        self.completed.notify_all();
    }

    pub(crate) fn notify_closed(&self) {
        let _wait = self.wait.lock();
        self.completed.notify_all();
    }

    #[cfg(test)]
    pub(crate) fn waiter_count(&self) -> usize {
        self.waiters.load(Ordering::Acquire)
    }
}

pub(crate) enum PrepareDecision {
    Existing {
        token: String,
        lifetime_key: String,
        generation: TopicGeneration,
    },
    Wait {
        initialization: InitializationPtr,
    },
    Initialize {
        initialization: InitializationPtr,
        generation: TopicGeneration,
    },
}

#[cfg(test)]
mod tests {
    use super::{PublishedTopics, shard_count_for};

    #[test]
    fn publication_shards_follow_the_configured_binding_capacity() {
        assert_eq!(shard_count_for(1), 64);
        assert_eq!(shard_count_for(4_096), 64);
        assert_eq!(shard_count_for(16_384), 256);
        assert_eq!(shard_count_for(100_000), 2_048);
        assert_eq!(shard_count_for(1_048_576), 16_384);

        assert_eq!(PublishedTopics::new(16_384).shards.len(), 256);
    }
}
