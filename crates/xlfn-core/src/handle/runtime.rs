use super::*;

pub(crate) struct TopicState {
    pub(crate) by_key: HashMap<String, Topic>,
    pub(crate) by_excel_id: HashMap<HandleTopicOwner, String>,
    pub(crate) initializing: HashMap<String, Arc<Initialization>>,
    pub(crate) generation: u64,
    pub(crate) closed: bool,
}

impl Default for TopicState {
    fn default() -> Self {
        Self {
            by_key: HashMap::new(),
            by_excel_id: HashMap::new(),
            initializing: HashMap::new(),
            generation: 1,
            closed: false,
        }
    }
}

pub(crate) struct Initialization {
    pub(crate) owner: ThreadId,
    pub(crate) owner_done: AtomicBool,
    pub(crate) completed: Condvar,
}

pub(crate) enum PrepareDecision {
    Existing {
        token: String,
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

/// Runtime-owned handle topics. Application code never inserts or removes
/// entries directly; generated UDF boundaries and Excel RTD callbacks do so.
pub(crate) struct HandleRuntime {
    pub(crate) registry: HandleRegistry,
    pub(crate) topics: Mutex<TopicState>,
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
            topics: Mutex::new(TopicState::default()),
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
    pub fn prepare<T>(
        &self,
        key: String,
        create: impl FnOnce() -> XllResult<Arc<T>>,
    ) -> XllResult<(String, bool)>
    where
        T: ExcelHandleObject,
    {
        self.prepare_observed(key, create, |_, _| Ok(()))
    }

    pub(crate) fn observe_existing(
        &self,
        key: &str,
        token: String,
        generation: u64,
        observe: impl FnOnce(&str, &str) -> XllResult<()>,
    ) -> XllResult<(String, bool)> {
        observe(key, &token)?;

        let topics = self.topics.lock();

        if topics.closed || topics.generation != generation {
            return Err(XllError::Closing);
        }

        if !topics
            .by_key
            .get(key)
            .is_some_and(|topic| topic.token == token)
        {
            return Err(XllError::StaleHandle);
        }

        Ok((token, false))
    }

    pub(crate) fn prepare_observed<T>(
        &self,
        key: String,
        create: impl FnOnce() -> XllResult<Arc<T>>,
        observe: impl FnOnce(&str, &str) -> XllResult<()>,
    ) -> XllResult<(String, bool)>
    where
        T: ExcelHandleObject,
    {
        let _active_initialization = HandleInitializationGuard::enter()?;
        let _prepare = self.prepares.enter();
        let _handle_operation = self.leases.acquire();

        let decision = loop {
            let mut topics = self.topics.lock();

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

                initialization.completed.wait(&mut topics);
                continue;
            }

            //
            // 2. No initialization is in flight, so a visible topic is committed
            //    enough to use as the memoized value.
            //
            if let Some(topic) = topics.by_key.get(&key) {
                break PrepareDecision::Existing {
                    token: topic.token.clone(),
                    generation: topics.generation,
                };
            }

            //
            // 3. Real miss. Become the single-flight owner.
            //
            let initialization = Arc::new(Initialization {
                owner: std::thread::current().id(),
                owner_done: AtomicBool::new(false),
                completed: Condvar::new(),
            });

            topics
                .initializing
                .insert(key.clone(), Arc::clone(&initialization));

            break PrepareDecision::Initialize {
                initialization,
                generation: topics.generation,
            };
        };

        let (initialization, generation) = match decision {
            PrepareDecision::Existing { token, generation } => {
                return self.observe_existing(&key, token, generation, observe);
            }

            PrepareDecision::Initialize {
                initialization,
                generation,
            } => (initialization, generation),
        };

        let initializing = scopeguard::guard(
            (&self.topics, key.as_str(), Arc::clone(&initialization)),
            |(topics, key, owned)| {
                let mut topics = topics.lock();
                let removed = topics
                    .initializing
                    .get(key)
                    .filter(|current| Arc::ptr_eq(current, &owned))
                    .is_some()
                    .then(|| topics.initializing.remove(key))
                    .flatten();
                drop(topics);
                owned.owner_done.store(true, Ordering::Release);
                if let Some(initialization) = removed {
                    initialization.completed.notify_all();
                } else {
                    owned.completed.notify_all();
                }
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
            (&self.registry, &self.topics, key.as_str(), token.as_str()),
            |(registry, topics, key, token)| {
                let mut topics = topics.lock();
                if let Some(topic) = topics.by_key.get(key).filter(|topic| topic.token == token) {
                    if let Some(owner) = topic.excel_topic {
                        topics.by_excel_id.remove(&owner);
                    }
                    topics.by_key.remove(key);
                }
                drop(topics);
                registry.remove_and_drop(token, "handle publication rollback");
            },
        );

        let mut topics = self.topics.lock();
        if topics.closed || topics.generation != generation {
            return Err(XllError::Closing);
        }
        topics.by_key.insert(
            key.clone(),
            Topic {
                token: token.clone(),
                #[cfg(any(target_os = "windows", test))]
                server_generation: None,
                excel_topic: None,
                #[cfg(any(target_os = "windows", test))]
                excel_topic_committed: false,
            },
        );
        drop(topics);

        {
            let topics = self.topics.lock();
            if topics.closed || topics.generation != generation {
                return Err(XllError::Closing);
            }
        }
        observe(&key, &token)?;

        let topics = self.topics.lock();
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
        let _ = scopeguard::ScopeGuard::into_inner(unpublished);
        drop(topics);
        drop(initializing);
        Ok((token, true))
    }

    #[cfg(any(target_os = "windows", test))]
    pub fn claim_server(&self, key: &str, server_generation: u64) -> XllResult<()> {
        let mut topics = self.topics.lock();
        if topics.closed {
            return Err(XllError::Closing);
        }
        let topic = topics.by_key.get_mut(key).ok_or(XllError::StaleHandle)?;
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
        key: &str,
    ) -> XllResult<String> {
        let owner = HandleTopicOwner {
            server_generation,
            topic_id: excel_topic_id,
        };
        let (token, created) = self.connect_inner(server_generation, excel_topic_id, key)?;
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
        key: &str,
    ) -> XllResult<HandleConnection> {
        let owner = HandleTopicOwner {
            server_generation,
            topic_id: excel_topic_id,
        };
        let (token, created) = self.connect_inner(server_generation, excel_topic_id, key)?;
        Ok(HandleConnection {
            runtime: Arc::downgrade(self),
            owner,
            key: key.to_owned(),
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
        key: &str,
    ) -> XllResult<(String, bool)> {
        let owner = HandleTopicOwner {
            server_generation,
            topic_id: excel_topic_id,
        };
        let mut topics = self.topics.lock();
        if topics.closed {
            return Err(XllError::Closing);
        }
        if topics
            .by_excel_id
            .get(&owner)
            .is_some_and(|existing| existing != key)
        {
            return Err(XllError::InvalidHandle);
        }
        let (token, created) = {
            let topic = topics.by_key.get_mut(key).ok_or(XllError::StaleHandle)?;
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
        topics.by_excel_id.insert(owner, key.to_owned());
        Ok((token, created))
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn commit_connection(&self, owner: HandleTopicOwner, key: &str) -> XllResult<()> {
        let mut topics = self.topics.lock();
        if topics.closed {
            return Err(XllError::Closing);
        }
        if topics.by_excel_id.get(&owner).map(String::as_str) != Some(key) {
            return Err(XllError::StaleHandle);
        }
        let topic = topics.by_key.get_mut(key).ok_or(XllError::StaleHandle)?;
        if topic.excel_topic != Some(owner) {
            return Err(XllError::StaleHandle);
        }
        topic.excel_topic_committed = true;
        Ok(())
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn rollback_connection(&self, owner: HandleTopicOwner, key: &str) {
        let mut topics = self.topics.lock();
        if topics.by_excel_id.get(&owner).map(String::as_str) != Some(key)
            || !topics.by_key.get(key).is_some_and(|topic| {
                topic.excel_topic == Some(owner) && !topic.excel_topic_committed
            })
        {
            return;
        }
        topics.by_excel_id.remove(&owner);
        if let Some(topic) = topics.by_key.get_mut(key) {
            // The formula already owns the object and token. Roll back only
            // the COM topic assignment so a failed value write can be retried.
            topic.excel_topic = None;
            topic.excel_topic_committed = false;
        }
    }

    #[cfg(test)]
    pub fn rollback(&self, key: &str) {
        let token = {
            let mut topics = self.topics.lock();
            let Some(topic) = topics.by_key.remove(key) else {
                return;
            };
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
            let mut topics = self.topics.lock();
            let Some(key) = topics.by_excel_id.remove(&owner) else {
                return;
            };
            topics.by_key.remove(&key).map(|topic| topic.token)
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
            let mut topics = self.topics.lock();

            topics.closed = true;
            topics.generation = topics.generation.wrapping_add(1);
            topics.by_key.clear();
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
            initialization.completed.notify_all();
        }

        //
        // Preserve the current cold-owner synchronization.
        //
        for initialization in initializations {
            let mut topics = self.topics.lock();

            while !initialization.owner_done.load(Ordering::Acquire) {
                initialization.completed.wait(&mut topics);
            }
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
            let mut topics = self.topics.lock();
            let keys = topics
                .by_key
                .iter()
                .filter(|(_, topic)| topic.server_generation == Some(server_generation))
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>();
            keys.into_iter()
                .filter_map(|key| {
                    let topic = topics.by_key.remove(&key)?;
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
            let mut topics = self.topics.lock();
            let tokens = topics
                .by_key
                .drain()
                .map(|(_, topic)| topic.token)
                .collect::<Vec<_>>();
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
