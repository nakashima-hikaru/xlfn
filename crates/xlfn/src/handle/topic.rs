//! The handle topic table and its publication snapshots.
//!
//! A topic has two representations with different synchronization needs:
//! [`TopicTableState`] is the canonical mutable state used by connection and
//! shutdown transitions, while [`PublishedTopics`] is the immutable, sharded
//! read snapshot used by the synchronous prepare hot path.  Keeping both
//! under one owner makes the publication protocol explicit: every snapshot
//! update is paired with a mutation of the canonical table while its lock is
//! held.

#[cfg(any(target_os = "windows", test))]
use super::FormulaLifetimeGeneration;
use super::{FormulaBinding, FormulaObserverId, HandleTopicKey, Topic};
use crate::generation::TopicGeneration;
use crate::{XllError, XllResult};
use arc_swap::ArcSwapAny;
use parking_lot::{Condvar, Mutex, RwLock};
#[cfg(test)]
use parking_lot::{RwLockReadGuard, RwLockWriteGuard};
use rustc_hash::FxHashMap;
use rustc_hash::FxHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
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
    pub(crate) binding: FormulaBinding,
    pub(crate) token: String,
    pub(crate) lifetime_key: Arc<str>,
    pub(crate) state: AtomicU8,
}

impl PublishedTopic {
    pub(crate) fn new(binding: FormulaBinding, token: String, lifetime_key: Arc<str>) -> Self {
        Self {
            binding,
            token,
            lifetime_key,
            state: AtomicU8::new(PublishedTopicState::Provisional as u8),
        }
    }

    pub(crate) fn state(&self) -> PublishedTopicState {
        PublishedTopicState::from_raw(self.state.load(Ordering::Acquire))
    }
}

pub(crate) type PublishedTopicMap = FxHashMap<HandleTopicKey, triomphe::Arc<PublishedTopic>>;
pub(crate) type PublishedTopicMapArc = triomphe::Arc<PublishedTopicMap>;

pub(crate) struct PublishedTopics {
    shards: Box<[ArcSwapAny<PublishedTopicMapArc>]>,
    shard_mask: usize,
}

impl PublishedTopics {
    pub(crate) fn new(maximum_bindings: usize) -> Self {
        let shard_count = shard_count_for(maximum_bindings);
        let empty_map = triomphe::Arc::new(PublishedTopicMap::default());
        Self {
            shards: (0..shard_count)
                .map(|_| ArcSwapAny::new(triomphe::Arc::clone(&empty_map)))
                .collect(),
            shard_mask: shard_count - 1,
        }
    }

    fn shard_index(&self, key: &HandleTopicKey) -> usize {
        let mut hasher = FxHasher::default();
        key.hash(&mut hasher);
        (hasher.finish() as usize) & self.shard_mask
    }

    pub(crate) fn load(&self, key: &HandleTopicKey) -> arc_swap::Guard<PublishedTopicMapArc> {
        self.shards[self.shard_index(key)].load()
    }

    /// Update the publication snapshot while holding the canonical topic lock.
    pub(crate) fn insert(&self, key: HandleTopicKey, topic: triomphe::Arc<PublishedTopic>) {
        let shard = &self.shards[self.shard_index(&key)];
        let current = shard.load_full();
        let mut next = current.as_ref().clone();
        next.insert(key, topic);
        shard.store(triomphe::Arc::new(next));
    }

    /// Update the publication snapshot while holding the canonical topic lock.
    pub(crate) fn remove(&self, key: HandleTopicKey) {
        let shard = &self.shards[self.shard_index(&key)];
        let current = shard.load_full();
        if !current.contains_key(&key) {
            return;
        }
        let mut next = current.as_ref().clone();
        next.remove(&key);
        shard.store(triomphe::Arc::new(next));
    }

    /// Clear all publication snapshots while holding the canonical topic lock.
    pub(crate) fn clear(&self) {
        let empty_map = triomphe::Arc::new(PublishedTopicMap::default());
        for shard in &self.shards {
            shard.store(triomphe::Arc::clone(&empty_map));
        }
    }
}

fn shard_count_for(maximum_bindings: usize) -> usize {
    let required = maximum_bindings.max(1).div_ceil(TARGET_TOPICS_PER_SHARD);
    required.next_power_of_two().max(MIN_PUBLISHED_TOPIC_SHARDS)
}

pub(crate) struct TopicTableState {
    pub(crate) by_key: FxHashMap<HandleTopicKey, Topic>,
    // Excel RTD callback strings are resolved here; they are not lifecycle
    // identities and are never parsed back into formula components.
    pub(crate) by_lifetime_key: FxHashMap<Arc<str>, HandleTopicKey>,
    pub(crate) by_observer_id: FxHashMap<FormulaObserverId, HandleTopicKey>,
    pub(crate) initializing: FxHashMap<HandleTopicKey, Arc<Initialization>>,
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

/// Canonical owner of all topic indices and their read-side publication view.
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

    /// Inspect or reserve a formula topic using the table's single-flight
    /// protocol.  Waiting is returned as data so the caller can release all
    /// table access before blocking on the initializer.
    pub(crate) fn prepare_decision(
        &self,
        key: HandleTopicKey,
        owner: ThreadId,
        make_initialization: impl FnOnce() -> Arc<Initialization>,
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
                lifetime_key: Arc::clone(&topic.publication.lifetime_key),
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
                lifetime_key: Arc::clone(&topic.publication.lifetime_key),
                generation: state.generation,
            });
        }

        let initialization = make_initialization();
        let generation = state.generation;
        state.initializing.insert(key, Arc::clone(&initialization));
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
            // The formula already owns the object and token. Roll back only
            // the COM topic assignment so a failed value write can be retried.
            topic.observer = None;
            topic.observer_committed = false;
        }
        true
    }

    /// Install the provisional canonical topic and its reverse index.
    pub(crate) fn insert_provisional(
        &self,
        key: HandleTopicKey,
        generation: TopicGeneration,
        publication: triomphe::Arc<PublishedTopic>,
        on_linearized: impl FnOnce(),
    ) -> XllResult<Arc<str>> {
        let lifetime_key = Arc::clone(&publication.lifetime_key);
        let mut state = self.state.write();
        if state.closed || state.generation != generation {
            return Err(XllError::Closing);
        }
        if state.by_key.contains_key(&key)
            || state.by_lifetime_key.contains_key(lifetime_key.as_ref())
        {
            return Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::HANDLE_TOPIC_COLLISION,
            });
        }
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
        state.by_lifetime_key.insert(Arc::clone(&lifetime_key), key);
        on_linearized();
        Ok(lifetime_key)
    }

    /// Make a provisional publication visible only after its initializer is
    /// still the current single-flight owner.
    pub(crate) fn commit_publication(
        &self,
        key: HandleTopicKey,
        generation: TopicGeneration,
        initialization: &Arc<Initialization>,
        publication: &triomphe::Arc<PublishedTopic>,
        on_linearized: impl FnOnce(),
    ) -> XllResult<()> {
        let mut state = self.state.write();
        if state.closed || state.generation != generation {
            return Err(XllError::Closing);
        }
        let valid_topic = state.by_key.get(&key).is_some_and(|topic| {
            topic.publication.binding == publication.binding
                && topic.publication.token == publication.token
                && triomphe::Arc::ptr_eq(&topic.publication, publication)
        });
        if !valid_topic {
            return Err(XllError::StaleHandle);
        }
        if !state
            .initializing
            .get(&key)
            .is_some_and(|current| Arc::ptr_eq(current, initialization))
        {
            return Err(XllError::StaleHandle);
        }
        self.published
            .insert(key, triomphe::Arc::clone(publication));
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
        initialization: &Arc<Initialization>,
    ) -> bool {
        let mut state = self.state.write();
        if state
            .initializing
            .get(&key)
            .is_some_and(|current| Arc::ptr_eq(current, initialization))
        {
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
        let publication = state
            .by_key
            .get(&key)
            .map(|topic| triomphe::Arc::clone(&topic.publication))?;
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
            .remove(topic.publication.lifetime_key.as_ref());
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

    /// Close the canonical table and return cold initializers that must be
    /// woken outside the table lock.
    pub(crate) fn close(&self) -> Vec<Arc<Initialization>> {
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
}

impl Initialization {
    pub(crate) fn wait_until_done(&self) {
        let mut wait = self.wait.lock();
        while !self.owner_done.load(Ordering::Acquire) {
            self.completed.wait(&mut wait);
        }
    }

    pub(crate) fn wait_until_done_or_closed(&self, topics: &TopicTable) {
        let mut wait = self.wait.lock();
        while !self.owner_done.load(Ordering::Acquire) && !topics.is_closed() {
            self.completed.wait(&mut wait);
        }
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
}

pub(crate) enum PrepareDecision {
    Existing {
        token: String,
        lifetime_key: Arc<str>,
        generation: TopicGeneration,
    },
    Wait {
        initialization: Arc<Initialization>,
    },
    Initialize {
        initialization: Arc<Initialization>,
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
