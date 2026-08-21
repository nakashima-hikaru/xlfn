use super::*;
use crate::generation::RuntimeGeneration;
#[cfg(any(target_os = "windows", test))]
use crate::generation::ServerGeneration;
use std::cell::OnceCell;
use std::mem::ManuallyDrop;

pub(crate) enum PreparedHandleObject {
    New {
        object_id: Option<ObjectId>,
        value: ErasedObject,
    },
    Existing {
        object: ObjectLocator,
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

/// Owns the single-flight marker for one cold topic preparation.
///
/// The marker is removed by `commit_publication` on success.  If any earlier
/// step fails, dropping this reservation removes the marker and wakes all
/// waiters, so the rollback protocol is no longer encoded in a closure hidden
/// in the middle of `prepare_observed_object`.
struct TopicReservation<'runtime> {
    runtime: &'runtime HandleRuntime,
    key: HandleTopicKey,
    initialization: Arc<Initialization>,
    active: bool,
}

impl<'runtime> TopicReservation<'runtime> {
    fn new(
        runtime: &'runtime HandleRuntime,
        key: HandleTopicKey,
        initialization: Arc<Initialization>,
    ) -> Self {
        Self {
            runtime,
            key,
            initialization,
            active: true,
        }
    }

    fn commit(mut self) {
        self.active = false;
    }
}

impl Drop for TopicReservation<'_> {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        #[cfg(any(test, feature = "handle-refinement-trace"))]
        if self
            .runtime
            .topics
            .finish_initialization(self.key, &self.initialization)
        {
            self.runtime
                .refinement
                .linearize()
                .finish_initializer(self.initialization.refinement_id);
        }
        #[cfg(not(any(test, feature = "handle-refinement-trace")))]
        let _ = self
            .runtime
            .topics
            .finish_initialization(self.key, &self.initialization);
        self.initialization.complete();
    }
}

/// Owns a binding and its provisional topic until publication is committed.
///
/// The object registry and topic table are intentionally rolled back together:
/// a provisional token must never survive a failed observation or a topic
/// collision.  This is the cold-path transaction boundary for handle
/// publication.
struct ProvisionalPublication<'runtime> {
    runtime: &'runtime HandleRuntime,
    key: HandleTopicKey,
    token: String,
    #[cfg(any(test, feature = "handle-refinement-trace"))]
    refinement_id: u64,
    active: bool,
}

impl<'runtime> ProvisionalPublication<'runtime> {
    fn new(
        runtime: &'runtime HandleRuntime,
        key: HandleTopicKey,
        token: String,
        #[cfg(any(test, feature = "handle-refinement-trace"))] refinement_id: u64,
    ) -> Self {
        Self {
            runtime,
            key,
            token,
            #[cfg(any(test, feature = "handle-refinement-trace"))]
            refinement_id,
            active: true,
        }
    }

    fn commit(mut self) {
        self.active = false;
    }
}

impl Drop for ProvisionalPublication<'_> {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        #[cfg(any(test, feature = "handle-refinement-trace"))]
        {
            let removed = self
                .runtime
                .topics
                .remove_topic_if_token(self.key, &self.token)
                .is_some();
            if removed {
                let token_wire = self.runtime.refinement_token(&self.token);
                let refinement = &self.runtime.refinement;
                let key = self.key;
                let refinement_id = self.refinement_id;
                let token = &self.token;
                let _ = self.runtime.registry.remove_and_drop_with_trace(
                    token,
                    "handle publication rollback",
                    move |reusable| {
                        refinement.rollback_pending(&key, refinement_id, reusable, token_wire);
                    },
                );
            } else {
                let _ = self.runtime.registry.remove_and_drop_with_trace(
                    &self.token,
                    "handle publication rollback",
                    |_| {},
                );
            }
        }
        #[cfg(not(any(test, feature = "handle-refinement-trace")))]
        {
            self.runtime
                .topics
                .remove_topic_if_token(self.key, &self.token);
            self.runtime
                .registry
                .remove_and_drop_with_kind(&self.token, "handle publication rollback");
        }
    }
}

/// Runtime-owned handle topics. Application code never inserts or removes
/// entries directly; generated UDF boundaries and Excel RTD callbacks do so.
pub(crate) struct HandleRuntime {
    pub(crate) registry: HandleRegistry,
    pub(crate) topics: TopicTable,
    pub(crate) prepares: HandlePrepareState,
    pub(crate) _module_ingress: Option<&'static crate::ingress::ExportIngress>,
    #[cfg(any(test, feature = "handle-refinement-trace"))]
    pub(crate) refinement: HandleRefinementHooks,
}

impl HandleRuntime {
    #[cfg(test)]
    pub fn try_new(maximum_bindings: usize) -> XllResult<Self> {
        Self::try_new_with_ingress(maximum_bindings, None)
    }

    pub(crate) fn try_new_with_ingress(
        maximum_bindings: usize,
        module_ingress: Option<&'static crate::ingress::ExportIngress>,
    ) -> XllResult<Self> {
        let registry = HandleRegistry::try_new(maximum_bindings)?;
        #[cfg(any(test, feature = "handle-refinement-trace"))]
        let registry_session = registry.codec.session;
        Ok(Self {
            registry,
            topics: TopicTable::new(),
            prepares: HandlePrepareState::new(),
            _module_ingress: module_ingress,
            #[cfg(any(test, feature = "handle-refinement-trace"))]
            refinement: HandleRefinementHooks::new(registry_session),
        })
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn set_ghost(&self, ghost: crate::shutdown_refinement::GhostHandle) {
        self.registry.set_ghost(ghost);
    }

    #[cfg(any(test, feature = "handle-refinement-trace"))]
    fn refinement_token(&self, token: &str) -> TokenWire {
        let parsed = self
            .registry
            .codec
            .parse(
                std::ptr::from_ref(&self.registry).addr(),
                HandleToken::new(token),
            )
            .expect("H4 trace token must be authenticated");
        TokenWire {
            session: self.registry.codec.session,
            slot: u64::from(parsed.id.slot),
            generation: parsed.id.generation.get(),
        }
    }

    #[cfg(test)]
    pub(crate) fn refinement_trace_json(&self) -> String {
        self.refinement.trace_json()
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
    pub fn new(maximum_bindings: usize) -> Self {
        Self::try_new(maximum_bindings).expect("test host provides an OS CSPRNG")
    }

    #[cfg(test)]
    pub fn prepare<T, K>(
        &self,
        key: K,
        create: impl FnOnce() -> XllResult<T>,
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
        generation: TopicGeneration,
        observe: impl FnOnce(&str, &str) -> XllResult<()>,
    ) -> XllResult<(String, bool)> {
        observe(&rtd_key, &token)?;
        self.topics.is_current(key, generation, &token)?;
        Ok((token, false))
    }

    fn commit_publication(
        &self,
        key: HandleTopicKey,
        generation: TopicGeneration,
        initialization: &Arc<Initialization>,
        publication: &triomphe::Arc<PublishedTopic>,
    ) -> XllResult<()> {
        // A provisional snapshot lets readers that raced with the publication
        // fall back to the canonical single-flight path. Make it Live only
        // after the initialization marker is removed.
        #[cfg(any(test, feature = "handle-refinement-trace"))]
        let token_wire = self.refinement_token(&publication.token);
        self.topics
            .commit_publication(key, generation, initialization, publication, || {
                #[cfg(any(test, feature = "handle-refinement-trace"))]
                {
                    let mut linearization = self.refinement.linearize();
                    linearization.commit_and_activate(
                        &key,
                        initialization.refinement_id,
                        token_wire,
                    );
                    linearization.finish_initializer(initialization.refinement_id);
                }
            })?;

        initialization.complete();
        Ok(())
    }

    pub(crate) fn prepare_observed<T, K>(
        &self,
        key: K,
        create: impl FnOnce() -> XllResult<T>,
        observe: impl FnOnce(&str, &str) -> XllResult<()>,
    ) -> XllResult<(String, bool)>
    where
        T: ExcelHandleObject,
        K: Into<HandleTopicKey>,
    {
        self.prepare_observed_object::<T, K>(
            key,
            || {
                create().map(|value| PreparedHandleObject::New {
                    object_id: None,
                    value: ErasedObject::new(value, Arc::clone(&self.registry.cleanup)),
                })
            },
            observe,
        )
    }

    pub(crate) fn prepare_observed_alias<T, K>(
        &self,
        key: K,
        object: ObjectLocator,
        observe: impl FnOnce(&str, &str) -> XllResult<()>,
    ) -> XllResult<(String, bool)>
    where
        T: ExcelHandleObject,
        K: Into<HandleTopicKey>,
    {
        self.prepare_observed_object::<T, K>(
            key,
            || Ok(PreparedHandleObject::Existing { object }),
            observe,
        )
    }

    fn prepare_observed_object<T, K>(
        &self,
        key: K,
        create: impl FnOnce() -> XllResult<PreparedHandleObject>,
        observe: impl FnOnce(&str, &str) -> XllResult<()>,
    ) -> XllResult<(String, bool)>
    where
        T: ExcelHandleObject,
        K: Into<HandleTopicKey>,
    {
        let key = key.into();
        let _active_initialization = HandleInitializationGuard::enter()?;
        let _prepare = self.prepares.try_enter().ok_or(XllError::Closing)?;
        #[cfg(any(test, feature = "handle-refinement-trace"))]
        let _refinement_prepare = self.refinement.prepare_guard();
        #[cfg(any(test, feature = "handle-refinement-trace"))]
        self.refinement.begin_prepare();
        {
            let published = self.topics.published().load(&key);
            if let Some(publication) = published.get(&key) {
                #[cfg(any(test, feature = "handle-refinement-trace"))]
                let warm_reader_id = {
                    let mut linearization = self.refinement.linearize();
                    (publication.state() == PublishedTopicState::Live)
                        .then(|| linearization.begin_warm_read(&key))
                };
                #[cfg(any(test, feature = "handle-refinement-trace"))]
                let warm_reader = warm_reader_id.is_some();
                #[cfg(not(any(test, feature = "handle-refinement-trace")))]
                let warm_reader = publication.state() == PublishedTopicState::Live;

                if warm_reader {
                    #[cfg(any(test, feature = "handle-refinement-trace"))]
                    let reader_id = warm_reader_id.expect("warm reader existence was checked");
                    let observed = observe(&publication.rtd_key, &publication.token);
                    #[allow(
                        clippy::question_mark,
                        reason = "the refinement trace must classify the failed warm read before returning"
                    )]
                    if let Err(error) = observed {
                        #[cfg(any(test, feature = "handle-refinement-trace"))]
                        let mut linearization = self.refinement.linearize();
                        #[cfg(any(test, feature = "handle-refinement-trace"))]
                        match publication.state() {
                            PublishedTopicState::Live => linearization.fail_warm_read(reader_id),
                            PublishedTopicState::Stale | PublishedTopicState::Closing => {
                                linearization.abandon_warm_read(reader_id)
                            }
                            PublishedTopicState::Provisional => {}
                        }
                        return Err(error);
                    }

                    #[cfg(any(test, feature = "handle-refinement-trace"))]
                    let result = {
                        let mut linearization = self.refinement.linearize();
                        match publication.state() {
                            PublishedTopicState::Live => {
                                linearization.finish_warm_read(reader_id);
                                Ok((publication.token.clone(), false))
                            }
                            PublishedTopicState::Closing => {
                                linearization.abandon_warm_read(reader_id);
                                Err(XllError::Closing)
                            }
                            PublishedTopicState::Provisional | PublishedTopicState::Stale => {
                                linearization.abandon_warm_read(reader_id);
                                Err(XllError::StaleHandle)
                            }
                        }
                    };
                    #[cfg(not(any(test, feature = "handle-refinement-trace")))]
                    let result = match publication.state() {
                        PublishedTopicState::Live => Ok((publication.token.clone(), false)),
                        PublishedTopicState::Closing => Err(XllError::Closing),
                        PublishedTopicState::Provisional | PublishedTopicState::Stale => {
                            Err(XllError::StaleHandle)
                        }
                    };
                    return result;
                }
            }
        }

        let owner = std::thread::current().id();
        let decision = loop {
            let decision = self.topics.prepare_decision(key, owner, || {
                #[cfg(any(test, feature = "handle-refinement-trace"))]
                let refinement_id = self.refinement.allocate_initializer_id();
                Arc::new(Initialization {
                    owner,
                    owner_done: AtomicBool::new(false),
                    wait: Mutex::new(()),
                    completed: Condvar::new(),
                    #[cfg(any(test, feature = "handle-refinement-trace"))]
                    refinement_id,
                })
            })?;
            match decision {
                PrepareDecision::Wait { initialization } => {
                    initialization.wait_until_done_or_closed(&self.topics);
                }
                PrepareDecision::Initialize {
                    initialization,
                    generation,
                } => {
                    #[cfg(any(test, feature = "handle-refinement-trace"))]
                    self.refinement
                        .begin_initializer(&key, initialization.refinement_id);
                    break PrepareDecision::Initialize {
                        initialization,
                        generation,
                    };
                }
                existing => break existing,
            }
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

            PrepareDecision::Wait { .. } => unreachable!("wait decisions never leave the loop"),
        };

        let reservation = TopicReservation::new(self, key, Arc::clone(&initialization));

        //
        // Cold path: no existing topic, invoke the factory.
        //
        let (token, binding_id, object_id, reused) = match create()? {
            PreparedHandleObject::New { object_id, value } => {
                let mut pending = PendingHandleValue::new(
                    &self.registry,
                    value,
                    "unpublished handle formula value",
                );
                self.registry
                    .insert_pending_object_with_kind::<T>(pending.slot(), object_id)?
            }
            PreparedHandleObject::Existing { object } => {
                self.registry.insert_existing_object_binding::<T>(object)?
            }
        };
        let binding = FormulaBinding {
            id: binding_id,
            object_id,
        };
        #[cfg(not(any(test, feature = "handle-refinement-trace")))]
        let _ = reused;
        #[cfg(any(test, feature = "handle-refinement-trace"))]
        {
            let parsed = self.refinement_token(&token);
            if reused {
                self.refinement.insert_pending_reuse(
                    &key,
                    initialization.refinement_id,
                    parsed.slot,
                    parsed.generation,
                );
            } else {
                self.refinement
                    .insert_pending_fresh(&key, initialization.refinement_id);
            }
        }
        #[cfg(any(test, feature = "handle-refinement-trace"))]
        let refinement_id = initialization.refinement_id;
        let provisional = ProvisionalPublication::new(
            self,
            key,
            token.clone(),
            #[cfg(any(test, feature = "handle-refinement-trace"))]
            refinement_id,
        );

        let rtd_key: Arc<str> = key.format_rtd_key().into();
        let publication = triomphe::Arc::new(PublishedTopic::new(
            binding,
            token.clone(),
            Arc::clone(&rtd_key),
        ));
        self.topics.insert_provisional(
            key,
            generation,
            triomphe::Arc::clone(&publication),
            || {
                #[cfg(any(test, feature = "handle-refinement-trace"))]
                self.refinement.publish_and_install(
                    &key,
                    initialization.refinement_id,
                    self.refinement_token(&token),
                    &rtd_key,
                );
            },
        )?;
        self.topics.is_current(key, generation, &token)?;
        observe(&rtd_key, &token)?;

        self.topics.is_current(key, generation, &token)?;
        self.commit_publication(key, generation, &initialization, &publication)?;
        provisional.commit();
        reservation.commit();
        Ok((token, true))
    }

    #[cfg(any(target_os = "windows", test))]
    pub fn claim_server(
        &self,
        rtd_key: &str,
        server_generation: ServerGeneration,
    ) -> XllResult<()> {
        let key = self.topics.claim_server(rtd_key, server_generation)?;
        #[cfg(any(test, feature = "handle-refinement-trace"))]
        self.refinement.claim_server(&key, server_generation);
        Ok(())
    }

    #[cfg(test)]
    pub fn connect(
        &self,
        server_generation: ServerGeneration,
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
        server_generation: ServerGeneration,
        excel_topic_id: i32,
        rtd_key: &str,
    ) -> XllResult<HandleConnection<'_>> {
        let owner = HandleTopicOwner {
            server_generation,
            topic_id: excel_topic_id,
        };
        let (key, token, created) =
            self.connect_inner(server_generation, excel_topic_id, rtd_key)?;
        Ok(HandleConnection {
            runtime: self,
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
        server_generation: ServerGeneration,
        excel_topic_id: i32,
        rtd_key: &str,
    ) -> XllResult<(HandleTopicKey, String, bool)> {
        let owner = HandleTopicOwner {
            server_generation,
            topic_id: excel_topic_id,
        };
        let (key, token, created) = self.topics.connect(server_generation, owner, rtd_key)?;
        #[cfg(any(test, feature = "handle-refinement-trace"))]
        if created {
            self.refinement.begin_connection(&key, owner);
        } else {
            self.refinement.reuse_committed_connection(&key, owner);
        }
        Ok((key, token, created))
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn commit_connection(
        &self,
        owner: HandleTopicOwner,
        key: HandleTopicKey,
    ) -> XllResult<()> {
        self.topics.commit_connection(owner, key)?;
        #[cfg(any(test, feature = "handle-refinement-trace"))]
        self.refinement.commit_connection(&key, owner);
        Ok(())
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn rollback_connection(&self, owner: HandleTopicOwner, key: HandleTopicKey) {
        if !self.topics.rollback_connection(owner, key) {
            return;
        }
        #[cfg(any(test, feature = "handle-refinement-trace"))]
        self.refinement.rollback_connection(&key, owner);
    }

    #[cfg(test)]
    pub fn rollback(&self, rtd_key: &str) {
        if let Some(removed) = self.topics.remove_by_rtd_key(rtd_key) {
            self.registry
                .remove_and_drop(&removed.token, "handle topic rollback");
        }
    }

    #[cfg(any(target_os = "windows", test))]
    pub fn disconnect(&self, server_generation: ServerGeneration, excel_topic_id: i32) {
        let owner = HandleTopicOwner {
            server_generation,
            topic_id: excel_topic_id,
        };
        let Some(removed) = self.topics.remove_by_excel_owner(owner) else {
            return;
        };
        #[cfg(any(test, feature = "handle-refinement-trace"))]
        {
            let mut linearization = self.refinement.linearize();
            linearization.disconnect(&removed.key, owner);
            drop(linearization);
            let token_wire = self.refinement_token(&removed.token);
            let refinement = &self.refinement;
            if removed.was_provisional {
                if let Some(runtime_id) = removed.initialization_id {
                    let _ = self.registry.remove_and_drop_with_trace(
                        &removed.token,
                        "handle topic disconnect",
                        move |reusable| {
                            refinement.drain_pending(token_wire, runtime_id, reusable);
                        },
                    );
                } else {
                    let _ = self.registry.remove_and_drop_with_trace(
                        &removed.token,
                        "handle topic disconnect",
                        |_| {},
                    );
                }
            } else {
                let _ = self.registry.remove_and_drop_with_trace(
                    &removed.token,
                    "handle topic disconnect",
                    move |reusable| {
                        refinement.drain_published(token_wire, reusable);
                    },
                );
            }
        }
        #[cfg(not(any(test, feature = "handle-refinement-trace")))]
        self.registry
            .remove_and_drop_with_kind(&removed.token, "handle topic disconnect");
    }

    pub fn lookup<'call, T>(
        &self,
        scope: &'call crate::CallScope<'call>,
        token: &str,
    ) -> XllResult<Handle<'call, T>>
    where
        T: ExcelHandleObject,
    {
        self.registry.lookup_handle(scope, token)
    }

    pub fn seal(&self) -> XllResult<crate::shutdown::HandleRegistrySealed> {
        self.prepares.close_admission();
        self.registry.begin_close();
        let initializations = self.topics.close();
        #[cfg(any(test, feature = "handle-refinement-trace"))]
        self.refinement.linearize().seal_for_close();

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

        let result = self.registry.seal();
        #[cfg(any(test, feature = "handle-refinement-trace"))]
        {
            self.refinement.close_registry();
            self.refinement.finish_close();
            if result.is_ok() {
                self.refinement.mark_returned_success();
            }
        }
        result
    }

    #[cfg(any(target_os = "windows", test))]
    pub fn terminate_topics(&self, server_generation: ServerGeneration) {
        let removals = self.topics.remove_generation(server_generation);
        #[cfg(any(test, feature = "handle-refinement-trace"))]
        if !removals.is_empty() {
            self.refinement
                .linearize()
                .detach_generation(server_generation);
        }
        for removed in removals {
            #[cfg(any(test, feature = "handle-refinement-trace"))]
            if removed.was_provisional {
                if let Some(runtime_id) = removed.initialization_id {
                    let token_wire = self.refinement_token(&removed.token);
                    let refinement = &self.refinement;
                    let _ = self.registry.remove_and_drop_with_trace(
                        &removed.token,
                        "handle RTD termination",
                        move |reusable| {
                            refinement.drain_pending(token_wire, runtime_id, reusable);
                        },
                    );
                } else {
                    let _ = self.registry.remove_and_drop_with_trace(
                        &removed.token,
                        "handle RTD termination",
                        |_| {},
                    );
                }
            } else {
                let token_wire = self.refinement_token(&removed.token);
                let refinement = &self.refinement;
                let _ = self.registry.remove_and_drop_with_trace(
                    &removed.token,
                    "handle RTD termination",
                    move |reusable| {
                        refinement.drain_published(token_wire, reusable);
                    },
                );
            }
            #[cfg(not(any(test, feature = "handle-refinement-trace")))]
            self.registry
                .remove_and_drop_with_kind(&removed.token, "handle RTD termination");
        }
    }

    pub fn terminate_all_topics(&self) {
        let removals = self.topics.remove_all();
        for removed in removals {
            #[cfg(any(test, all(target_os = "windows", feature = "handle-refinement-trace")))]
            {
                let token_wire = self.refinement_token(&removed.token);
                let refinement = &self.refinement;
                if removed.was_provisional {
                    if let Some(runtime_id) = removed.initialization_id {
                        let _ = self.registry.remove_and_drop_with_trace(
                            &removed.token,
                            "handle RTD termination",
                            move |reusable| {
                                refinement.drain_pending(token_wire, runtime_id, reusable);
                            },
                        );
                    } else {
                        let _ = self.registry.remove_and_drop_with_trace(
                            &removed.token,
                            "handle RTD termination",
                            |_| {},
                        );
                    }
                } else {
                    let refinement = &self.refinement;
                    let _ = self.registry.remove_and_drop_with_trace(
                        &removed.token,
                        "handle RTD termination",
                        move |reusable| {
                            refinement.drain_published(token_wire, reusable);
                        },
                    );
                }
            }
            #[cfg(not(any(
                test,
                all(target_os = "windows", feature = "handle-refinement-trace")
            )))]
            self.registry
                .remove_and_drop_with_kind(&removed.token, "handle RTD termination");
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.registry.len()
    }
}

/// The handle runtime has stopped accepting work and its registry has moved
/// every payload root to the retired store. The token keeps the runtime alive
/// until add-in state cleanup has completed and pin quiescence is certified.
pub(crate) struct HandleRuntimeSealed {
    generation: Option<RuntimeGeneration>,
    runtime: Option<Arc<HandleRuntime>>,
    registry: Option<crate::shutdown::HandleRegistrySealed>,
}

impl HandleRuntimeSealed {
    fn empty(generation: Option<RuntimeGeneration>) -> Self {
        Self {
            generation,
            runtime: None,
            registry: None,
        }
    }

    fn from_runtime(
        generation: Option<RuntimeGeneration>,
        runtime: Arc<HandleRuntime>,
        registry: crate::shutdown::HandleRegistrySealed,
    ) -> Self {
        Self {
            generation,
            runtime: Some(runtime),
            registry: Some(registry),
        }
    }

    pub(crate) fn finish(self) -> XllResult<crate::shutdown::HandlesQuiescent> {
        if let (Some(runtime), Some(registry)) = (self.runtime, self.registry) {
            runtime.registry.finish_quiescence(&registry)?;
        }
        Ok(crate::shutdown::HandlesQuiescent::new(self.generation))
    }
}

pub(crate) struct HandleRuntimeSlot {
    published: arc_swap::ArcSwapOption<HandleRuntime>,
    state: Mutex<HandleRuntimeSlotState>,
    changed: parking_lot::Condvar,
    #[cfg(any(test, feature = "shutdown-refinement"))]
    ghost: std::sync::OnceLock<crate::shutdown_refinement::GhostHandle>,
}

type HandleRuntimeSlotState =
    crate::runtime_components::GenerationServiceState<crate::HandleConfig, HandleRuntime>;

/// A read capability that holds an `arc_swap::Guard` over a published
/// `HandleRuntime`.  The warm path acquires this without any `Mutex` or
/// `Arc::clone`.
pub(crate) struct HandleRuntimeRead {
    guard: arc_swap::Guard<Option<Arc<HandleRuntime>>>,
}

impl std::ops::Deref for HandleRuntimeRead {
    type Target = HandleRuntime;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.guard
            .as_ref()
            .expect("HandleRuntimeRead always contains a runtime")
            .as_ref()
    }
}

impl HandleRuntimeRead {
    /// Expose the underlying `Arc` for ownership-escape paths (RTD observe,
    /// `ensure_server`).
    #[inline]
    pub(crate) fn as_arc(&self) -> &Arc<HandleRuntime> {
        self.guard
            .as_ref()
            .expect("HandleRuntimeRead always contains a runtime")
    }
}

impl HandleRuntimeSlot {
    pub(crate) const fn new() -> Self {
        Self {
            published: arc_swap::ArcSwapOption::const_empty(),
            state: Mutex::new(HandleRuntimeSlotState::Closed),
            changed: parking_lot::Condvar::new(),
            #[cfg(any(test, feature = "shutdown-refinement"))]
            ghost: std::sync::OnceLock::new(),
        }
    }

    pub(crate) fn arm(
        &self,
        generation: RuntimeGeneration,
        config: crate::HandleConfig,
    ) -> XllResult<()> {
        let mut state = self.state.lock();
        if !matches!(*state, HandleRuntimeSlotState::Closed) {
            return Err(XllError::Closing);
        }
        *state = HandleRuntimeSlotState::Cold { generation, config };
        self.changed.notify_all();
        Ok(())
    }

    pub(crate) fn disarm(&self, generation: RuntimeGeneration) -> XllResult<()> {
        let mut state = self.state.lock();
        match &*state {
            HandleRuntimeSlotState::Cold {
                generation: active, ..
            } if *active == generation => {
                *state = HandleRuntimeSlotState::Closed;
                self.changed.notify_all();
                Ok(())
            }
            HandleRuntimeSlotState::Closed => Ok(()),
            _ => Err(XllError::Closing),
        }
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn set_ghost(&self, ghost: crate::shutdown_refinement::GhostHandle) {
        let _ = self.ghost.set(ghost.clone());
        let runtime = self.published.load();
        if let Some(runtime) = runtime.as_ref() {
            runtime.set_ghost(ghost);
        }
    }

    #[inline]
    pub(crate) fn is_none(&self) -> bool {
        if self.published.load().is_some() {
            return false;
        }
        matches!(
            *self.state.lock(),
            HandleRuntimeSlotState::Closed | HandleRuntimeSlotState::InitFaulted { .. }
        )
    }

    /// Acquire a read guard over the published `HandleRuntime`.
    ///
    /// The warm path (runtime already initialized) performs a single
    /// `ArcSwap::load` with no `Mutex` and no `Arc::clone`.
    #[inline]
    pub(crate) fn read(&self) -> XllResult<HandleRuntimeRead> {
        let guard = self.published.load();
        if guard.is_some() {
            return Ok(HandleRuntimeRead { guard });
        }
        drop(guard);
        self.read_slow()
    }

    #[cold]
    fn read_slow(&self) -> XllResult<HandleRuntimeRead> {
        let mut state = self.state.lock();

        loop {
            match &*state {
                HandleRuntimeSlotState::Ready { .. } => {
                    drop(state);
                    let guard = self.published.load();
                    debug_assert!(guard.is_some());
                    return Ok(HandleRuntimeRead { guard });
                }

                HandleRuntimeSlotState::InitFaulted { error, .. } => {
                    return Err(error.clone());
                }

                HandleRuntimeSlotState::TeardownFaulted { error, runtime, .. } => {
                    let _ = runtime;
                    return Err(error.clone());
                }

                HandleRuntimeSlotState::Initializing { generation }
                | HandleRuntimeSlotState::Sealing { generation } => {
                    let _ = generation;
                    self.changed.wait(&mut state);
                }

                HandleRuntimeSlotState::Cold { generation, .. } => {
                    let generation = *generation;
                    let config = match &*state {
                        HandleRuntimeSlotState::Cold { config, .. } => *config,
                        _ => unreachable!(),
                    };
                    *state = HandleRuntimeSlotState::Initializing { generation };
                    drop(state);

                    let candidate = HandleRuntime::try_new_with_ingress(
                        usize::try_from(config.maximum_bindings())
                            .expect("handle capacity fits the platform usize"),
                        Some(crate::ingress::global_ingress()),
                    )
                    .map(Arc::new);

                    let mut state = self.state.lock();
                    match candidate {
                        Ok(runtime) => {
                            #[cfg(any(test, feature = "shutdown-refinement"))]
                            if let Some(ghost) = self.ghost.get() {
                                runtime.set_ghost(Arc::clone(ghost));
                            }

                            self.published.store(Some(runtime));
                            *state = HandleRuntimeSlotState::Ready { generation };
                            self.changed.notify_all();
                            drop(state);

                            let guard = self.published.load();
                            debug_assert!(guard.is_some());
                            return Ok(HandleRuntimeRead { guard });
                        }

                        Err(error) => {
                            *state = HandleRuntimeSlotState::InitFaulted {
                                generation,
                                error: error.clone(),
                            };
                            self.changed.notify_all();
                            return Err(error);
                        }
                    }
                }

                HandleRuntimeSlotState::Closed => {
                    return Err(XllError::Closing);
                }
            }
        }
    }

    /// Owned `Arc` escape for test/benchmark code that needs to hold a
    /// `HandleRuntime` beyond a call scope.
    #[cfg(any(test, feature = "bench-internals"))]
    pub(crate) fn get_owned(&self) -> XllResult<Arc<HandleRuntime>> {
        let read = self.read()?;
        Ok(Arc::clone(read.as_arc()))
    }

    pub(crate) fn seal(
        &self,
        generation: Option<RuntimeGeneration>,
    ) -> XllResult<HandleRuntimeSealed> {
        let handles = {
            let mut state = self.state.lock();

            while matches!(
                *state,
                HandleRuntimeSlotState::Initializing { .. }
                    | HandleRuntimeSlotState::Sealing { .. }
            ) {
                self.changed.wait(&mut state);
            }

            match &*state {
                HandleRuntimeSlotState::Ready { generation: active } => {
                    if generation != Some(*active) {
                        return Err(XllError::Closing);
                    }
                    let handles = self.published.swap(None);
                    *state = HandleRuntimeSlotState::Sealing {
                        generation: *active,
                    };
                    handles
                }
                HandleRuntimeSlotState::Cold {
                    generation: active, ..
                }
                | HandleRuntimeSlotState::InitFaulted {
                    generation: active, ..
                } => {
                    if generation != Some(*active) {
                        return Err(XllError::Closing);
                    }
                    *state = HandleRuntimeSlotState::Closed;
                    self.changed.notify_all();
                    return Ok(HandleRuntimeSealed::empty(generation));
                }
                HandleRuntimeSlotState::Closed => {
                    return Ok(HandleRuntimeSealed::empty(generation));
                }
                HandleRuntimeSlotState::TeardownFaulted {
                    generation: active,
                    error,
                    runtime,
                    ..
                } => {
                    let _ = runtime;
                    if generation != Some(*active) {
                        return Err(XllError::Closing);
                    }
                    return Err(error.clone());
                }
                HandleRuntimeSlotState::Initializing { .. }
                | HandleRuntimeSlotState::Sealing { .. } => unreachable!(),
            }
        };

        let Some(handles) = handles else {
            return Err(XllError::Internal {
                diagnostic_id: crate::DiagnosticId::HANDLE_SLOT,
            });
        };
        let generation = generation.expect("a published handle runtime has a generation");

        let rtd_result = crate::rtd::shutdown(Arc::clone(&handles));
        let handle_result = handles.seal();
        let result = rtd_result.and(handle_result);
        let mut state = self.state.lock();
        match result {
            Ok(registry) => {
                *state = HandleRuntimeSlotState::Closed;
                self.changed.notify_all();
                Ok(HandleRuntimeSealed::from_runtime(
                    Some(generation),
                    handles,
                    registry,
                ))
            }
            Err(error) => {
                *state = HandleRuntimeSlotState::TeardownFaulted {
                    generation,
                    error: error.clone(),
                    runtime: ManuallyDrop::new(handles),
                };
                self.changed.notify_all();
                Err(error)
            }
        }
    }
}

pub(crate) struct HandleRuntimeResolver<'call> {
    slot: &'call HandleRuntimeSlot,
    resolved: OnceCell<XllResult<HandleRuntimeRead>>,
}

impl<'call> HandleRuntimeResolver<'call> {
    #[inline]
    pub(crate) fn new(slot: &'call HandleRuntimeSlot) -> Self {
        Self {
            slot,
            resolved: OnceCell::new(),
        }
    }

    /// Returns a shared reference to the `HandleRuntime`.
    ///
    /// The first call performs an `ArcSwap::load`; subsequent calls within the
    /// same UDF invocation return the cached guard with zero atomic operations.
    #[inline]
    pub(crate) fn get(&self) -> XllResult<&HandleRuntime> {
        match self.resolved.get_or_init(|| self.slot.read()) {
            Ok(runtime) => Ok(runtime),
            Err(error) => Err(error.clone()),
        }
    }

    /// Returns a reference to the underlying `Arc` for paths that need
    /// ownership escape (RTD observation, `ensure_server`).
    #[inline]
    pub(crate) fn get_arc(&self) -> XllResult<&Arc<HandleRuntime>> {
        match self.resolved.get_or_init(|| self.slot.read()) {
            Ok(runtime) => Ok(runtime.as_arc()),
            Err(error) => Err(error.clone()),
        }
    }
}
