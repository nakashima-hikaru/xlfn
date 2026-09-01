//! Runtime-owned topic and initializer arenas.
//!
//! Published topic and single-flight initializer allocations are retained as
//! tombstones until the handle service is reclaimed. Read-side maps publish
//! copyable pointers only; they never share ownership or run reclamation.

#![allow(
    unsafe_code,
    reason = "topic publication uses stable non-owning pointers into table-owned arenas"
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
use std::ops::Deref;
use std::ptr::NonNull;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::thread::ThreadId;

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
pub(crate) struct PublishedTopicPtr(NonNull<PublishedTopic>);

impl PublishedTopicPtr {
    fn from_ref(topic: &PublishedTopic) -> Self {
        Self(NonNull::from(topic))
    }

    pub(crate) fn get(self) -> &'static PublishedTopic {
        // SAFETY: TopicTable retains every publication allocation until the
        // service is quiescent and reclaimed.
        unsafe { self.0.as_ref() }
    }
}

impl Deref for PublishedTopicPtr {
    type Target = PublishedTopic;

    fn deref(&self) -> &Self::Target {
        self.get()
    }
}

unsafe impl Send for PublishedTopicPtr {}
unsafe impl Sync for PublishedTopicPtr {}

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

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) struct InitializationPtr(NonNull<Initialization>);

impl InitializationPtr {
    fn from_ref(initialization: &Initialization) -> Self {
        Self(NonNull::from(initialization))
    }

    pub(crate) fn get(self) -> &'static Initialization {
        // SAFETY: initializers are retained in TopicTableState's arena until
        // service reclamation, after all prepare operations are drained.
        unsafe { self.0.as_ref() }
    }
}

impl Deref for InitializationPtr {
    type Target = Initialization;

    fn deref(&self) -> &Self::Target {
        self.get()
    }
}

unsafe impl Send for InitializationPtr {}
unsafe impl Sync for InitializationPtr {}

pub(crate) struct TopicTableState {
    pub(crate) by_key: FxHashMap<HandleTopicKey, Topic>,
    pub(crate) by_lifetime_key: FxHashMap<String, HandleTopicKey>,
    pub(crate) by_observer_id: FxHashMap<FormulaObserverId, HandleTopicKey>,
    pub(crate) initializing: FxHashMap<HandleTopicKey, InitializationPtr>,
    publications: Vec<Box<PublishedTopic>>,
    initializations: Vec<Box<Initialization>>,
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
            publications: Vec::new(),
            initializations: Vec::new(),
            generation: TopicGeneration::ONE,
            closed: false,
        }
    }
}

pub(crate) struct TopicTable {
    state: RwLock<TopicTableState>,
    published: PublishedTopics,
}

impl TopicTable {
    pub(crate) fn new(maximum_bindings: usize) -> Self {
        Self {
            state: RwLock::new(TopicTableState::default()),
            published: PublishedTopics::new(maximum_bindings),
        }
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
        if let Some(initialization) = state.initializing.get(&key).copied() {
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
        if let Some(initialization) = state.initializing.get(&key).copied() {
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

        let allocation = Box::new(make_initialization());
        let initialization = InitializationPtr::from_ref(allocation.as_ref());
        state.initializations.push(allocation);
        let generation = state.generation;
        state.initializing.insert(key, initialization);
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
        let pointer = PublishedTopicPtr::from_ref(publication.as_ref());
        state.publications.push(publication);
        state.by_key.insert(
            key,
            Topic {
                publication: pointer,
                #[cfg(any(target_os = "windows", test))]
                lifetime_generation: None,
                observer: None,
                #[cfg(any(target_os = "windows", test))]
                observer_committed: false,
            },
        );
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
        if !state
            .by_key
            .get(&key)
            .is_some_and(|topic| topic.publication == publication)
            || state.initializing.get(&key).copied() != Some(initialization)
        {
            return Err(XllError::StaleHandle);
        }
        self.published.insert(key, publication);
        state.initializing.remove(&key);
        publication
            .state
            .store(PublishedTopicState::Live as u8, Ordering::Release);
        on_linearized();
        Ok(())
    }

    pub(crate) fn finish_initialization(
        &self,
        key: HandleTopicKey,
        initialization: InitializationPtr,
    ) -> bool {
        let mut state = self.state.write();
        if state.initializing.get(&key).copied() == Some(initialization) {
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
    ) -> Option<TopicRemoval> {
        let publication = state.by_key.get(&key)?.publication;
        let was_provisional = publication.state() == PublishedTopicState::Provisional;
        let initialization_id = state
            .initializing
            .get(&key)
            .map(|initialization| initialization.refinement_id);
        publication
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
        Some(TopicRemoval {
            token: topic.publication.token.clone(),
            key,
            was_provisional,
            initialization_id,
        })
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn remove_by_observer(&self, owner: FormulaObserverId) -> Option<TopicRemoval> {
        let mut state = self.state.write();
        let key = state.by_observer_id.remove(&owner)?;
        self.remove_topic_locked(&mut state, key)
    }

    #[cfg(test)]
    pub(crate) fn remove_by_lifetime_key(&self, lifetime_key: &str) -> Option<TopicRemoval> {
        let mut state = self.state.write();
        let key = state.by_lifetime_key.get(lifetime_key).copied()?;
        self.remove_topic_locked(&mut state, key)
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
        let removal = self.remove_topic_locked(&mut state, key);
        if removal.is_some() {
            on_linearized();
        }
        removal
    }

    pub(crate) fn close(&self) -> Vec<InitializationPtr> {
        let mut state = self.state.write();
        state.closed = true;
        state.generation = state.generation.next().unwrap_or(state.generation);
        for topic in state.by_key.values() {
            topic
                .publication
                .state
                .store(PublishedTopicState::Closing as u8, Ordering::Release);
        }
        self.published.clear();
        state.by_key.clear();
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
        keys.into_iter()
            .filter_map(|key| self.remove_topic_locked(&mut state, key))
            .collect()
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
        keys.into_iter()
            .filter_map(|key| self.remove_topic_locked(&mut state, key))
            .collect()
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
