use super::*;
use arc_swap::ArcSwap;
use rustc_hash::FxHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::AtomicU8;

const PUBLISHED_TOPIC_SHARD_COUNT: usize = 64;

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
    pub(crate) rtd_key: Arc<str>,
    pub(crate) state: AtomicU8,
}

impl PublishedTopic {
    fn new(token: String, rtd_key: Arc<str>) -> Self {
        Self {
            token,
            rtd_key,
            state: AtomicU8::new(PublishedTopicState::Provisional as u8),
        }
    }

    fn state(&self) -> PublishedTopicState {
        PublishedTopicState::from_raw(self.state.load(Ordering::Acquire))
    }
}

pub(crate) type PublishedTopicMap = FxHashMap<HandleTopicKey, Arc<PublishedTopic>>;

pub(crate) struct PublishedTopics {
    shards: [ArcSwap<PublishedTopicMap>; PUBLISHED_TOPIC_SHARD_COUNT],
}

impl PublishedTopics {
    fn new() -> Self {
        Self {
            shards: std::array::from_fn(|_| ArcSwap::from_pointee(PublishedTopicMap::default())),
        }
    }

    fn shard_index(key: &HandleTopicKey) -> usize {
        let mut hasher = FxHasher::default();
        key.hash(&mut hasher);
        (hasher.finish() as usize) & (PUBLISHED_TOPIC_SHARD_COUNT - 1)
    }

    pub(crate) fn load(&self, key: &HandleTopicKey) -> arc_swap::Guard<Arc<PublishedTopicMap>> {
        self.shards[Self::shard_index(key)].load()
    }

    /// Update the publication snapshot while holding the canonical topic lock.
    fn insert(&self, key: HandleTopicKey, topic: Arc<PublishedTopic>) {
        let shard = &self.shards[Self::shard_index(&key)];
        let current = shard.load_full();
        let mut next = current.as_ref().clone();
        next.insert(key, topic);
        shard.store(Arc::new(next));
    }

    /// Update the publication snapshot while holding the canonical topic lock.
    fn remove(&self, key: HandleTopicKey) {
        let shard = &self.shards[Self::shard_index(&key)];
        let current = shard.load_full();
        if !current.contains_key(&key) {
            return;
        }
        let mut next = current.as_ref().clone();
        next.remove(&key);
        shard.store(Arc::new(next));
    }

    /// Clear all publication snapshots while holding the canonical topic lock.
    fn clear(&self) {
        for shard in &self.shards {
            shard.store(Arc::new(PublishedTopicMap::default()));
        }
    }
}

pub(crate) struct TopicState {
    pub(crate) by_key: FxHashMap<HandleTopicKey, Topic>,
    // Excel RTD callback strings are resolved here; they are not lifecycle
    // identities and are never parsed back into formula components.
    pub(crate) by_rtd_key: FxHashMap<Arc<str>, HandleTopicKey>,
    pub(crate) by_excel_id: FxHashMap<HandleTopicOwner, HandleTopicKey>,
    pub(crate) initializing: FxHashMap<HandleTopicKey, Arc<Initialization>>,
    pub(crate) generation: u64,
    pub(crate) closed: bool,
}

pub(crate) const HANDLE_TOPIC_RTD_KEY_COLLISION_DIAGNOSTIC_ID: u64 = 0x4841_4e44_5254_4443;

impl Default for TopicState {
    fn default() -> Self {
        Self {
            by_key: FxHashMap::default(),
            by_rtd_key: FxHashMap::default(),
            by_excel_id: FxHashMap::default(),
            initializing: FxHashMap::default(),
            generation: 1,
            closed: false,
        }
    }
}

pub(crate) struct Initialization {
    pub(crate) owner: ThreadId,
    pub(crate) owner_done: AtomicBool,
    pub(crate) wait: Mutex<()>,
    pub(crate) completed: Condvar,
}

impl Initialization {
    fn wait_until_done(&self) {
        let mut wait = self.wait.lock();
        while !self.owner_done.load(Ordering::Acquire) {
            self.completed.wait(&mut wait);
        }
    }

    fn wait_until_done_or_closed(&self, topics: &RwLock<TopicState>) {
        let mut wait = self.wait.lock();
        while !self.owner_done.load(Ordering::Acquire) && !topics.read().closed {
            self.completed.wait(&mut wait);
        }
    }

    fn complete(&self) {
        let _wait = self.wait.lock();
        self.owner_done.store(true, Ordering::Release);
        self.completed.notify_all();
    }

    fn notify_closed(&self) {
        let _wait = self.wait.lock();
        self.completed.notify_all();
    }
}

pub(crate) enum PrepareDecision {
    Existing {
        token: String,
        rtd_key: Arc<str>,
        generation: u64,
    },
    Initialize {
        initialization: Arc<Initialization>,
        generation: u64,
    },
}

thread_local! {
    static ACTIVE_HANDLE_INITIALIZATION_DEPTH: Cell<u32> = const { Cell::new(0) };
}

pub(crate) struct HandleInitializationGuard;

impl HandleInitializationGuard {
    pub(crate) fn enter() -> XllResult<Self> {
        ACTIVE_HANDLE_INITIALIZATION_DEPTH.with(|depth| {
            if depth.get() != 0 {
                return Err(XllError::ReentrantCall);
            }
            depth.set(1);
            Ok(Self)
        })
    }
}

impl Drop for HandleInitializationGuard {
    fn drop(&mut self) {
        ACTIVE_HANDLE_INITIALIZATION_DEPTH.with(|depth| {
            debug_assert_eq!(depth.get(), 1);
            depth.set(0);
        });
    }
}

#[cfg(any(test, feature = "shutdown-refinement"))]
struct HandleOperationGhostGuard<'a> {
    leases: &'a HandleLeaseState,
}

#[cfg(any(test, feature = "shutdown-refinement"))]
impl<'a> HandleOperationGhostGuard<'a> {
    fn enter(leases: &'a HandleLeaseState) -> Self {
        leases.record_ghost_event(crate::shutdown_refinement::GhostEvent::BeginHandleOperation);
        Self { leases }
    }
}

#[cfg(any(test, feature = "shutdown-refinement"))]
impl Drop for HandleOperationGhostGuard<'_> {
    fn drop(&mut self) {
        self.leases
            .record_ghost_event(crate::shutdown_refinement::GhostEvent::EndHandleOperation);
    }
}

/// Runtime-owned handle topics. Application code never inserts or removes
/// entries directly; generated UDF boundaries and Excel RTD callbacks do so.
pub(crate) struct HandleRuntime {
    pub(crate) registry: HandleRegistry,
    pub(crate) topics: RwLock<TopicState>,
    pub(crate) published: PublishedTopics,
    pub(crate) prepares: HandlePrepareState,
    pub(crate) leases: Arc<HandleLeaseState>,
    pub(crate) _module_ingress: Option<&'static crate::ingress::ExportIngress>,
}

impl HandleRuntime {
    #[cfg(test)]
    pub fn try_new(maximum_handles: usize) -> XllResult<Self> {
        Self::try_new_with_ingress(maximum_handles, None)
    }

    pub(crate) fn try_new_with_ingress(
        maximum_handles: usize,
        module_ingress: Option<&'static crate::ingress::ExportIngress>,
    ) -> XllResult<Self> {
        Ok(Self {
            registry: HandleRegistry::try_new(maximum_handles)?,
            topics: RwLock::new(TopicState::default()),
            published: PublishedTopics::new(),
            prepares: HandlePrepareState::new(),
            leases: Arc::new(HandleLeaseState::new()),
            _module_ingress: module_ingress,
        })
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn set_ghost(&self, ghost: crate::shutdown_refinement::GhostHandle) {
        self.registry.set_ghost(Arc::clone(&ghost));
        self.leases.set_ghost(ghost);
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn begin_rtd_operation(&self) -> XllResult<RtdOperationGuard> {
        #[cfg(any(test, feature = "shutdown-refinement"))]
        let ghost = self.registry.ghost_handle();

        let ingress_guard = if let Some(ingress) = self._module_ingress {
            let (guard, accepted) = ingress.enter_with(|| {
                #[cfg(any(test, feature = "shutdown-refinement"))]
                if let Some(ghost) = ghost.as_ref() {
                    ghost.record_event(crate::shutdown_refinement::GhostEvent::BeginRtdOperation);
                }
            });
            if !accepted {
                return Err(XllError::Closing);
            }
            Some(guard)
        } else {
            None
        };

        Ok(RtdOperationGuard {
            _ingress_guard: ingress_guard,
            #[cfg(any(test, feature = "shutdown-refinement"))]
            ghost,
        })
    }

    #[cfg(test)]
    #[must_use]
    pub fn new(maximum_handles: usize) -> Self {
        Self::try_new(maximum_handles).expect("test host provides an OS CSPRNG")
    }

    #[cfg(test)]
    pub fn prepare<T, K>(
        &self,
        key: K,
        create: impl FnOnce() -> XllResult<Arc<T>>,
    ) -> XllResult<(String, bool)>
    where
        T: ExcelHandleObject,
        K: Into<HandleTopicKey>,
    {
        self.prepare_observed(key, create, |_, _| Ok(()))
    }

    pub(crate) fn observe_existing(
        &self,
        key: HandleTopicKey,
        rtd_key: Arc<str>,
        token: String,
        generation: u64,
        observe: impl FnOnce(&str, &str) -> XllResult<()>,
    ) -> XllResult<(String, bool)> {
        observe(&rtd_key, &token)?;

        let topics = self.topics.read();

        if topics.closed || topics.generation != generation {
            return Err(XllError::Closing);
        }

        if !topics
            .by_key
            .get(&key)
            .is_some_and(|topic| topic.token == token)
        {
            return Err(XllError::StaleHandle);
        }

        Ok((token, false))
    }

    fn commit_publication(
        &self,
        key: HandleTopicKey,
        generation: u64,
        initialization: &Arc<Initialization>,
        publication: &Arc<PublishedTopic>,
    ) -> XllResult<()> {
        let mut topics = self.topics.write();

        if topics.closed || topics.generation != generation {
            return Err(XllError::Closing);
        }

        let valid_topic = topics.by_key.get(&key).is_some_and(|topic| {
            topic.token == publication.token && Arc::ptr_eq(&topic.publication, publication)
        });
        if !valid_topic {
            return Err(XllError::StaleHandle);
        }

        if !topics
            .initializing
            .get(&key)
            .is_some_and(|current| Arc::ptr_eq(current, initialization))
        {
            return Err(XllError::StaleHandle);
        }

        // A provisional snapshot lets readers that raced with the publication
        // fall back to the canonical single-flight path. Make it Live only
        // after the initialization marker is removed.
        self.published.insert(key, Arc::clone(publication));
        topics.initializing.remove(&key);
        publication
            .state
            .store(PublishedTopicState::Live as u8, Ordering::Release);

        drop(topics);
        initialization.complete();
        Ok(())
    }

    pub(crate) fn prepare_observed<T, K>(
        &self,
        key: K,
        create: impl FnOnce() -> XllResult<Arc<T>>,
        observe: impl FnOnce(&str, &str) -> XllResult<()>,
    ) -> XllResult<(String, bool)>
    where
        T: ExcelHandleObject,
        K: Into<HandleTopicKey>,
    {
        let key = key.into();
        let _active_initialization = HandleInitializationGuard::enter()?;
        let _prepare = self.prepares.enter();
        #[cfg(any(test, feature = "shutdown-refinement"))]
        let _ghost_handle_operation = HandleOperationGhostGuard::enter(&self.leases);

        {
            let published = self.published.load(&key);
            if let Some(publication) = published.get(&key)
                && publication.state() == PublishedTopicState::Live
            {
                observe(&publication.rtd_key, &publication.token)?;

                return match publication.state() {
                    PublishedTopicState::Live => Ok((publication.token.clone(), false)),
                    PublishedTopicState::Closing => Err(XllError::Closing),
                    PublishedTopicState::Provisional | PublishedTopicState::Stale => {
                        Err(XllError::StaleHandle)
                    }
                };
            }
        }

        let decision = loop {
            let topics = self.topics.read();

            if topics.closed {
                return Err(XllError::Closing);
            }

            //
            // 1. A cold publication for this key is still in progress.
            //
            if let Some(initialization) = topics.initializing.get(&key).cloned() {
                if initialization.owner == std::thread::current().id() {
                    return Err(XllError::ReentrantCall);
                }

                drop(topics);
                initialization.wait_until_done_or_closed(&self.topics);
                continue;
            }

            //
            // 2. No initialization is in flight, so a visible topic is committed
            //    enough to use as the memoized value.
            //
            if let Some(topic) = topics.by_key.get(&key) {
                let decision = PrepareDecision::Existing {
                    token: topic.token.clone(),
                    rtd_key: Arc::clone(&topic.rtd_key),
                    generation: topics.generation,
                };
                drop(topics);
                break decision;
            }

            //
            // 3. Real miss. Become the single-flight owner.
            //
            drop(topics);
            let mut topics = self.topics.write();

            if topics.closed {
                return Err(XllError::Closing);
            }

            if let Some(initialization) = topics.initializing.get(&key).cloned() {
                if initialization.owner == std::thread::current().id() {
                    return Err(XllError::ReentrantCall);
                }

                drop(topics);
                initialization.wait_until_done_or_closed(&self.topics);
                continue;
            }

            if let Some(topic) = topics.by_key.get(&key) {
                let decision = PrepareDecision::Existing {
                    token: topic.token.clone(),
                    rtd_key: Arc::clone(&topic.rtd_key),
                    generation: topics.generation,
                };
                drop(topics);
                break decision;
            }

            let initialization = Arc::new(Initialization {
                owner: std::thread::current().id(),
                owner_done: AtomicBool::new(false),
                wait: Mutex::new(()),
                completed: Condvar::new(),
            });

            topics.initializing.insert(key, Arc::clone(&initialization));

            break PrepareDecision::Initialize {
                initialization,
                generation: topics.generation,
            };
        };

        let (initialization, generation) = match decision {
            PrepareDecision::Existing {
                token,
                rtd_key,
                generation,
            } => {
                return self.observe_existing(key, rtd_key, token, generation, observe);
            }

            PrepareDecision::Initialize {
                initialization,
                generation,
            } => (initialization, generation),
        };

        let initializing = scopeguard::guard(
            (&self.topics, key, Arc::clone(&initialization)),
            |(topics, key, owned)| {
                {
                    let mut topics = topics.write();
                    if topics
                        .initializing
                        .get(&key)
                        .is_some_and(|current| Arc::ptr_eq(current, &owned))
                    {
                        topics.initializing.remove(&key);
                    }
                }
                owned.complete();
            },
        );

        //
        // Cold path: no existing topic, invoke the factory.
        //
        let value = match create() {
            Ok(value) => value,
            Err(error) => {
                return Err(error);
            }
        };
        let mut value =
            PendingHandleValue::new(&self.registry, value, "unpublished handle formula value");

        let token = self.registry.insert_pending(value.slot())?;
        let unpublished = scopeguard::guard(
            (&self.registry, &self.topics, key, token.as_str()),
            |(registry, topics, key, token)| {
                let mut topics = topics.write();
                if let Some(topic) = topics.by_key.get(&key).filter(|topic| topic.token == token) {
                    topic
                        .publication
                        .state
                        .store(PublishedTopicState::Stale as u8, Ordering::Release);
                    // The publication is normally not visible until the
                    // final commit. Removing it here also covers future
                    // changes that add a post-publication failure point.
                    self.published.remove(key);
                    let rtd_key = Arc::clone(&topic.rtd_key);
                    let owner = topic.excel_topic;
                    topics.by_key.remove(&key);
                    topics.by_rtd_key.remove(rtd_key.as_ref());
                    if let Some(owner) = owner {
                        topics.by_excel_id.remove(&owner);
                    }
                }
                drop(topics);
                registry.remove_and_drop(token, "handle publication rollback");
            },
        );

        let mut topics = self.topics.write();
        if topics.closed || topics.generation != generation {
            return Err(XllError::Closing);
        }
        let rtd_key: Arc<str> = key.format_rtd_key().into();
        if topics.by_key.contains_key(&key) || topics.by_rtd_key.contains_key(rtd_key.as_ref()) {
            return Err(XllError::Internal {
                diagnostic_id: HANDLE_TOPIC_RTD_KEY_COLLISION_DIAGNOSTIC_ID,
            });
        }
        let publication = Arc::new(PublishedTopic::new(token.clone(), Arc::clone(&rtd_key)));
        topics.by_key.insert(
            key,
            Topic {
                token: token.clone(),
                rtd_key: Arc::clone(&rtd_key),
                publication: Arc::clone(&publication),
                #[cfg(any(target_os = "windows", test))]
                server_generation: None,
                excel_topic: None,
                #[cfg(any(target_os = "windows", test))]
                excel_topic_committed: false,
            },
        );
        topics.by_rtd_key.insert(rtd_key.clone(), key);
        drop(topics);

        {
            let topics = self.topics.read();
            if topics.closed || topics.generation != generation {
                return Err(XllError::Closing);
            }
        }
        observe(&rtd_key, &token)?;

        let topics = self.topics.read();
        if topics.closed || topics.generation != generation {
            return Err(XllError::Closing);
        }
        if !topics
            .by_key
            .get(&key)
            .is_some_and(|topic| topic.token == token)
        {
            return Err(XllError::StaleHandle);
        }
        drop(topics);
        self.commit_publication(key, generation, &initialization, &publication)?;
        let _ = scopeguard::ScopeGuard::into_inner(unpublished);
        let _ = scopeguard::ScopeGuard::into_inner(initializing);
        Ok((token, true))
    }

    #[cfg(any(target_os = "windows", test))]
    fn topic_key_for_rtd(topics: &TopicState, rtd_key: &str) -> XllResult<HandleTopicKey> {
        topics
            .by_rtd_key
            .get(rtd_key)
            .copied()
            .ok_or(XllError::StaleHandle)
    }

    #[cfg(any(target_os = "windows", test))]
    pub fn claim_server(&self, rtd_key: &str, server_generation: u64) -> XllResult<()> {
        let mut topics = self.topics.write();
        if topics.closed {
            return Err(XllError::Closing);
        }
        let key = Self::topic_key_for_rtd(&topics, rtd_key)?;
        let topic = topics.by_key.get_mut(&key).ok_or(XllError::StaleHandle)?;
        if topic
            .server_generation
            .is_some_and(|existing| existing != server_generation)
        {
            return Err(XllError::InvalidHandle);
        }
        topic.server_generation = Some(server_generation);
        Ok(())
    }

    #[cfg(test)]
    pub fn connect(
        &self,
        server_generation: u64,
        excel_topic_id: i32,
        rtd_key: &str,
    ) -> XllResult<String> {
        let owner = HandleTopicOwner {
            server_generation,
            topic_id: excel_topic_id,
        };
        let (key, token, created) =
            self.connect_inner(server_generation, excel_topic_id, rtd_key)?;
        if created && let Err(error) = self.commit_connection(owner, key) {
            self.rollback_connection(owner, key);
            return Err(error);
        }
        Ok(token)
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn connect_transaction(
        self: &Arc<Self>,
        server_generation: u64,
        excel_topic_id: i32,
        rtd_key: &str,
    ) -> XllResult<HandleConnection> {
        let owner = HandleTopicOwner {
            server_generation,
            topic_id: excel_topic_id,
        };
        let (key, token, created) =
            self.connect_inner(server_generation, excel_topic_id, rtd_key)?;
        Ok(HandleConnection {
            runtime: Arc::downgrade(self),
            owner,
            key,
            token,
            created,
            finished: false,
        })
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn connect_inner(
        &self,
        server_generation: u64,
        excel_topic_id: i32,
        rtd_key: &str,
    ) -> XllResult<(HandleTopicKey, String, bool)> {
        let owner = HandleTopicOwner {
            server_generation,
            topic_id: excel_topic_id,
        };
        let mut topics = self.topics.write();
        if topics.closed {
            return Err(XllError::Closing);
        }
        let key = Self::topic_key_for_rtd(&topics, rtd_key)?;
        if topics
            .by_excel_id
            .get(&owner)
            .is_some_and(|existing| existing != &key)
        {
            return Err(XllError::InvalidHandle);
        }
        let (token, created) = {
            let topic = topics.by_key.get_mut(&key).ok_or(XllError::StaleHandle)?;
            if topic
                .server_generation
                .is_some_and(|existing| existing != server_generation)
            {
                return Err(XllError::InvalidHandle);
            }
            topic.server_generation = Some(server_generation);
            let created = if let Some(existing) = topic.excel_topic {
                if existing != owner {
                    return Err(XllError::InvalidHandle);
                }
                if !topic.excel_topic_committed {
                    return Err(XllError::Overloaded);
                }
                false
            } else {
                topic.excel_topic = Some(owner);
                topic.excel_topic_committed = false;
                true
            };
            (topic.token.clone(), created)
        };
        topics.by_excel_id.insert(owner, key);
        Ok((key, token, created))
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn commit_connection(
        &self,
        owner: HandleTopicOwner,
        key: HandleTopicKey,
    ) -> XllResult<()> {
        let mut topics = self.topics.write();
        if topics.closed {
            return Err(XllError::Closing);
        }
        if topics.by_excel_id.get(&owner) != Some(&key) {
            return Err(XllError::StaleHandle);
        }
        let topic = topics.by_key.get_mut(&key).ok_or(XllError::StaleHandle)?;
        if topic.excel_topic != Some(owner) {
            return Err(XllError::StaleHandle);
        }
        topic.excel_topic_committed = true;
        Ok(())
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn rollback_connection(&self, owner: HandleTopicOwner, key: HandleTopicKey) {
        let mut topics = self.topics.write();
        if topics.by_excel_id.get(&owner) != Some(&key)
            || !topics.by_key.get(&key).is_some_and(|topic| {
                topic.excel_topic == Some(owner) && !topic.excel_topic_committed
            })
        {
            return;
        }
        topics.by_excel_id.remove(&owner);
        if let Some(topic) = topics.by_key.get_mut(&key) {
            // The formula already owns the object and token. Roll back only
            // the COM topic assignment so a failed value write can be retried.
            topic.excel_topic = None;
            topic.excel_topic_committed = false;
        }
    }

    #[cfg(test)]
    pub fn rollback(&self, rtd_key: &str) {
        let token = {
            let mut topics = self.topics.write();
            let Ok(key) = Self::topic_key_for_rtd(&topics, rtd_key) else {
                return;
            };
            let Some(publication) = topics
                .by_key
                .get(&key)
                .map(|topic| Arc::clone(&topic.publication))
            else {
                return;
            };
            publication
                .state
                .store(PublishedTopicState::Stale as u8, Ordering::Release);
            self.published.remove(key);
            let Some(topic) = topics.by_key.remove(&key) else {
                return;
            };
            topics.by_rtd_key.remove(topic.rtd_key.as_ref());
            if let Some(owner) = topic.excel_topic {
                topics.by_excel_id.remove(&owner);
            }
            Some(topic.token)
        };
        if let Some(token) = token {
            self.registry
                .remove_and_drop(&token, "handle topic rollback");
        }
    }

    #[cfg(any(target_os = "windows", test))]
    pub fn disconnect(&self, server_generation: u64, excel_topic_id: i32) {
        let owner = HandleTopicOwner {
            server_generation,
            topic_id: excel_topic_id,
        };
        let token = {
            let mut topics = self.topics.write();
            let Some(key) = topics.by_excel_id.remove(&owner) else {
                return;
            };
            let Some(publication) = topics
                .by_key
                .get(&key)
                .map(|topic| Arc::clone(&topic.publication))
            else {
                return;
            };
            publication
                .state
                .store(PublishedTopicState::Stale as u8, Ordering::Release);
            self.published.remove(key);
            let topic = topics.by_key.remove(&key);
            topic.map(|topic| {
                topics.by_rtd_key.remove(topic.rtd_key.as_ref());
                topic.token
            })
        };
        if let Some(token) = token {
            self.registry
                .remove_and_drop(&token, "handle topic disconnect");
        }
    }

    pub fn lookup<T>(&self, token: &str) -> XllResult<Handle<T>>
    where
        T: ExcelHandleObject,
    {
        self.registry.lookup_handle(token, &self.leases)
    }

    pub fn close(&self) -> XllResult<()> {
        let initializations = {
            let mut topics = self.topics.write();

            topics.closed = true;
            topics.generation = topics.generation.wrapping_add(1);
            for topic in topics.by_key.values() {
                topic
                    .publication
                    .state
                    .store(PublishedTopicState::Closing as u8, Ordering::Release);
            }
            self.published.clear();
            topics.by_key.clear();
            topics.by_rtd_key.clear();
            topics.by_excel_id.clear();

            topics
                .initializing
                .drain()
                .map(|(_, value)| value)
                .collect::<Vec<_>>()
        };

        //
        // Wake cold-path waiters.
        //
        for initialization in &initializations {
            initialization.notify_closed();
        }

        //
        // Preserve the current cold-owner synchronization.
        //
        for initialization in initializations {
            initialization.wait_until_done();
        }

        //
        // warm prepares are no longer represented in `initializing`.
        // Wait for every prepare_observed call that entered before or during
        // the close transition to leave before closing the registry.
        //
        self.prepares.wait_for_idle();

        self.registry.close_with_leases(&self.leases)
    }

    #[cfg(any(target_os = "windows", test))]
    pub fn terminate_topics(&self, server_generation: u64) {
        let tokens = {
            let mut topics = self.topics.write();
            let keys = topics
                .by_key
                .iter()
                .filter(|(_, topic)| topic.server_generation == Some(server_generation))
                .map(|(key, _)| *key)
                .collect::<Vec<_>>();
            keys.into_iter()
                .filter_map(|key| {
                    let publication = topics
                        .by_key
                        .get(&key)
                        .map(|topic| Arc::clone(&topic.publication))?;
                    publication
                        .state
                        .store(PublishedTopicState::Stale as u8, Ordering::Release);
                    self.published.remove(key);
                    let topic = topics.by_key.remove(&key)?;
                    topics.by_rtd_key.remove(topic.rtd_key.as_ref());
                    if let Some(owner) = topic.excel_topic {
                        topics.by_excel_id.remove(&owner);
                    }
                    Some(topic.token)
                })
                .collect::<Vec<_>>()
        };
        for token in tokens {
            self.registry
                .remove_and_drop(&token, "handle RTD termination");
        }
    }

    pub fn terminate_all_topics(&self) {
        let tokens = {
            let mut topics = self.topics.write();
            for topic in topics.by_key.values() {
                topic
                    .publication
                    .state
                    .store(PublishedTopicState::Stale as u8, Ordering::Release);
            }
            self.published.clear();
            let tokens = topics
                .by_key
                .drain()
                .map(|(_, topic)| topic.token)
                .collect::<Vec<_>>();
            topics.by_rtd_key.clear();
            topics.by_excel_id.clear();
            tokens
        };
        for token in tokens {
            self.registry
                .remove_and_drop(&token, "handle RTD termination");
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.registry.len()
    }
}
