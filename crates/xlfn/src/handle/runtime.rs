#[cfg(target_os = "windows")]
use super::RtdOperationGuard;
use super::{
    ErasedObject, ExcelHandleObject, FormulaBinding, Handle, HandlePrepareState,
    HandleRefinementHooks, HandleStore, HandleTopicKey, Initialization, ObjectId, ObjectLocator,
    PrepareDecision, PublishedTopic, PublishedTopicState, TopicRemoval, TopicTable,
};
#[cfg(any(target_os = "windows", test))]
use super::{HandleConnection, HandleTopicOwner};
use crate::generation::RuntimeGeneration;
#[cfg(any(target_os = "windows", test))]
use crate::generation::ServerGeneration;
use crate::generation::TopicGeneration;
use crate::shutdown::HandleRegistrySealed;
use crate::{XllError, XllResult};
use parking_lot::{Condvar, Mutex};
use std::cell::Cell;
use std::cell::OnceCell;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

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
        if ACTIVE_HANDLE_INITIALIZATION_DEPTH.get() != 0 {
            return Err(XllError::ReentrantCall);
        }
        ACTIVE_HANDLE_INITIALIZATION_DEPTH.set(1);
        Ok(Self)
    }
}

impl Drop for HandleInitializationGuard {
    fn drop(&mut self) {
        debug_assert_eq!(ACTIVE_HANDLE_INITIALIZATION_DEPTH.get(), 1);
        ACTIVE_HANDLE_INITIALIZATION_DEPTH.set(0);
    }
}

/// Owns the single-flight marker for one cold topic preparation.
///
/// The marker is removed by `commit_publication` on success.  If any earlier
/// step fails, dropping this reservation removes the marker and wakes all
/// waiters, so the rollback protocol is no longer encoded in a closure hidden
/// in the middle of `prepare_observed_object`.
struct TopicReservation<'runtime> {
    runtime: &'runtime FormulaHandleService,
    key: HandleTopicKey,
    initialization: Arc<Initialization>,
    active: bool,
}

impl<'runtime> TopicReservation<'runtime> {
    fn new(
        runtime: &'runtime FormulaHandleService,
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
        if self
            .runtime
            .topics
            .finish_initialization(self.key, &self.initialization)
        {
            self.runtime
                .refinement
                .observe_finish_initializer(self.initialization.refinement_id);
        }
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
    runtime: &'runtime FormulaHandleService,
    key: HandleTopicKey,
    token: String,
    refinement_id: u64,
    active: bool,
}

impl<'runtime> ProvisionalPublication<'runtime> {
    fn new(
        runtime: &'runtime FormulaHandleService,
        key: HandleTopicKey,
        token: String,
        refinement_id: u64,
    ) -> Self {
        Self {
            runtime,
            key,
            token,
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
        let token_wire = self.runtime.refinement_token(&self.token);
        let refinement = &self.runtime.refinement;
        let key = self.key;
        let refinement_id = self.refinement_id;
        let removed = self
            .runtime
            .topics
            .remove_topic_if_token(self.key, &self.token, || {
                refinement.observe_withdraw_and_invalidate(&key, refinement_id, token_wire);
            })
            .is_some();
        let _ = self.runtime.store.remove_and_drop_with_observer(
            &self.token,
            "handle publication rollback",
            move |reusable| {
                if removed {
                    refinement.observe_rollback_pending(&key, refinement_id, reusable, token_wire);
                }
            },
        );
    }
}

/// Runtime-owned handle topics. Application code never inserts or removes
/// entries directly; generated UDF boundaries and Excel RTD callbacks do so.
pub(crate) struct FormulaHandleService {
    pub(super) store: HandleStore,
    pub(super) topics: TopicTable,
    pub(super) prepares: HandlePrepareState,
    pub(super) _module_ingress: Option<&'static crate::ingress::ExportIngress>,
    pub(super) refinement: HandleRefinementHooks,
}

impl FormulaHandleService {
    #[cfg(test)]
    pub fn try_new(maximum_bindings: usize) -> XllResult<Self> {
        Self::try_new_with_ingress(maximum_bindings, None)
    }

    pub(crate) fn try_new_with_ingress(
        maximum_bindings: usize,
        module_ingress: Option<&'static crate::ingress::ExportIngress>,
    ) -> XllResult<Self> {
        let store = HandleStore::try_new(maximum_bindings)?;
        let registry_session = store.session();
        Ok(Self {
            store,
            topics: TopicTable::new(),
            prepares: HandlePrepareState::new(),
            _module_ingress: module_ingress,
            refinement: HandleRefinementHooks::new(registry_session),
        })
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn set_ghost(&self, ghost: crate::shutdown_refinement::GhostHandle) {
        self.store.set_ghost(ghost);
    }

    fn refinement_token(&self, token: &str) -> super::TokenWire {
        self.store.refinement_token(token)
    }

    #[cfg(test)]
    pub(crate) fn refinement_trace_json(&self) -> String {
        self.refinement.trace_json()
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn begin_rtd_operation(&self) -> XllResult<RtdOperationGuard> {
        #[cfg(any(test, feature = "shutdown-refinement"))]
        let ghost = self.store.ghost_handle();

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
        let token_wire = self.refinement_token(&publication.token);
        self.topics
            .commit_publication(key, generation, initialization, publication, || {
                self.refinement.observe_commit_and_activate(
                    &key,
                    initialization.refinement_id,
                    token_wire,
                );
                self.refinement
                    .observe_finish_initializer(initialization.refinement_id);
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
                    value: self.store.erase(value),
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
        let _refinement_prepare = self.refinement.observe_prepare();
        {
            let published = self.topics.published().load(&key);
            if let Some(publication) = published.get(&key) {
                let warm_reader = publication.state() == PublishedTopicState::Live;
                let warm_reader_id =
                    warm_reader.then(|| self.refinement.observe_begin_warm_read(&key));

                if warm_reader {
                    let reader_id = warm_reader_id.expect("warm reader existence was checked");
                    let observed = observe(&publication.rtd_key, &publication.token);
                    if let Err(error) = observed {
                        match publication.state() {
                            PublishedTopicState::Live => {
                                self.refinement.observe_fail_warm_read(reader_id);
                            }
                            PublishedTopicState::Stale | PublishedTopicState::Closing => {
                                self.refinement.observe_abandon_warm_read(reader_id);
                            }
                            PublishedTopicState::Provisional => {}
                        }
                        return Err(error);
                    }

                    return match publication.state() {
                        PublishedTopicState::Live => {
                            self.refinement.observe_finish_warm_read(reader_id);
                            Ok((publication.token.clone(), false))
                        }
                        PublishedTopicState::Closing => {
                            self.refinement.observe_abandon_warm_read(reader_id);
                            Err(XllError::Closing)
                        }
                        PublishedTopicState::Provisional | PublishedTopicState::Stale => {
                            self.refinement.observe_abandon_warm_read(reader_id);
                            Err(XllError::StaleHandle)
                        }
                    };
                }
            }
        }

        let owner = std::thread::current().id();
        let decision = loop {
            let decision = self.topics.prepare_decision(key, owner, || {
                let refinement_id = self.refinement.observe_allocate_initializer_id();
                Arc::new(Initialization {
                    owner,
                    owner_done: AtomicBool::new(false),
                    wait: Mutex::new(()),
                    completed: Condvar::new(),
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
                    self.refinement
                        .observe_begin_initializer(&key, initialization.refinement_id);
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
                self.store.insert_pending::<T>(value, object_id)?
            }
            PreparedHandleObject::Existing { object } => self.store.insert_existing::<T>(object)?,
        };
        let binding = FormulaBinding {
            id: binding_id,
            object_id,
        };
        let parsed = self.refinement_token(&token);
        if reused {
            self.refinement.observe_insert_pending_reuse(
                &key,
                initialization.refinement_id,
                parsed.slot,
                parsed.generation,
            );
        } else {
            self.refinement
                .observe_insert_pending_fresh(&key, initialization.refinement_id);
        }
        let provisional =
            ProvisionalPublication::new(self, key, token.clone(), initialization.refinement_id);

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
                self.refinement.observe_publish_and_install(
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
        let _key = self.topics.claim_server(rtd_key, server_generation)?;
        self.refinement
            .observe_claim_server(&_key, server_generation);
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
        if created {
            self.refinement.observe_begin_connection(&key, owner);
        } else {
            self.refinement
                .observe_reuse_committed_connection(&key, owner);
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
        self.refinement.observe_commit_connection(&key, owner);
        Ok(())
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn rollback_connection(&self, owner: HandleTopicOwner, key: HandleTopicKey) {
        if !self.topics.rollback_connection(owner, key) {
            return;
        }
        self.refinement.observe_rollback_connection(&key, owner);
    }

    #[cfg(test)]
    pub fn rollback(&self, rtd_key: &str) {
        if let Some(removed) = self.topics.remove_by_rtd_key(rtd_key) {
            self.remove_topic_value(&removed, "handle topic rollback");
        }
    }

    fn remove_topic_value(&self, removed: &TopicRemoval, operation: &'static str) {
        let _key = removed.key;
        let token_wire = self.refinement_token(&removed.token);
        let refinement = &self.refinement;
        let was_provisional = removed.was_provisional;
        let initialization_id = removed.initialization_id;
        let _ =
            self.store
                .remove_and_drop_with_observer(&removed.token, operation, move |reusable| {
                    if was_provisional {
                        if let Some(runtime_id) = initialization_id {
                            refinement.observe_drain_pending(token_wire, runtime_id, reusable);
                        }
                    } else {
                        refinement.observe_drain_published(token_wire, reusable);
                    }
                });
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
        self.refinement.observe_disconnect(&removed.key, owner);
        self.remove_topic_value(&removed, "handle topic disconnect");
    }

    pub fn lookup<'call, T>(
        &self,
        scope: &'call crate::call::CallScope<'call>,
        token: &str,
    ) -> XllResult<Handle<'call, T>>
    where
        T: ExcelHandleObject,
    {
        self.store.lookup(scope, token)
    }

    pub fn seal(&self) -> XllResult<crate::shutdown::HandleRegistrySealed> {
        self.prepares.close_admission();
        self.store.begin_close();
        let initializations = self.topics.close();
        self.refinement.observe_seal_for_close();

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

        let result = self.store.seal();
        self.refinement.observe_close_registry();
        self.refinement.observe_finish_close();
        if result.is_ok() {
            self.refinement.observe_mark_returned_success();
        }
        result
    }

    #[cfg(any(target_os = "windows", test))]
    pub fn terminate_topics(&self, server_generation: ServerGeneration) {
        let removals = self.topics.remove_generation(server_generation);
        if !removals.is_empty() {
            self.refinement.observe_detach_generation(server_generation);
        }
        for removed in removals {
            self.remove_topic_value(&removed, "handle RTD termination");
        }
    }

    pub fn terminate_all_topics(&self) {
        let removals = self.topics.remove_all();
        for removed in removals {
            self.remove_topic_value(&removed, "handle RTD termination");
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.store.len()
    }
}

/// The handle runtime has stopped accepting work and its registry has moved
/// every payload root to the retired store. The token keeps the runtime alive
/// until add-in state cleanup has completed and pin quiescence is certified.
pub(crate) struct FormulaHandleServiceSealed {
    generation: Option<RuntimeGeneration>,
    service: Option<Arc<FormulaHandleService>>,
    registry: Option<HandleRegistrySealed>,
}

/// Proof that the handle registry for one specific runtime generation has no
/// remaining pins. The generation identity travels with the proof so a
/// certificate cannot be silently reused for a different service instance.
#[derive(Debug)]
pub(crate) struct HandleStoreQuiescent {
    generation: Option<RuntimeGeneration>,
}

impl HandleStoreQuiescent {
    pub(super) fn new(generation: Option<RuntimeGeneration>) -> Self {
        Self { generation }
    }

    #[cfg(test)]
    pub(crate) const fn for_test(generation: Option<RuntimeGeneration>) -> Self {
        Self { generation }
    }

    pub(crate) const fn generation(&self) -> Option<RuntimeGeneration> {
        self.generation
    }
}

impl FormulaHandleServiceSealed {
    fn empty(generation: Option<RuntimeGeneration>) -> Self {
        Self {
            generation,
            service: None,
            registry: None,
        }
    }

    fn from_service(
        generation: Option<RuntimeGeneration>,
        service: Arc<FormulaHandleService>,
        registry: crate::shutdown::HandleRegistrySealed,
    ) -> Self {
        Self {
            generation,
            service: Some(service),
            registry: Some(registry),
        }
    }

    pub(crate) fn finish(self) -> XllResult<HandleStoreQuiescent> {
        let generation = self.generation;
        if let (Some(service), Some(registry)) = (self.service, self.registry) {
            service.store.quiescent(&registry, generation)
        } else {
            Ok(HandleStoreQuiescent::new(generation))
        }
    }
}

pub(crate) struct FormulaHandleServiceSlot {
    service:
        crate::runtime_components::GenerationServiceSlot<crate::HandleConfig, FormulaHandleService>,
    #[cfg(any(test, feature = "shutdown-refinement"))]
    ghost: std::sync::OnceLock<crate::shutdown_refinement::GhostHandle>,
}

/// A read capability that holds an `arc_swap::Guard` over a published
/// `FormulaHandleService`.  The warm path acquires this without any `Mutex` or
/// `Arc::clone`.
pub(crate) type FormulaHandleServiceRead =
    crate::runtime_components::GenerationServiceRead<FormulaHandleService>;

impl FormulaHandleServiceSlot {
    pub(crate) const fn new() -> Self {
        Self {
            service: crate::runtime_components::GenerationServiceSlot::new(),
            #[cfg(any(test, feature = "shutdown-refinement"))]
            ghost: std::sync::OnceLock::new(),
        }
    }

    pub(crate) fn arm(
        &self,
        generation: RuntimeGeneration,
        config: crate::HandleConfig,
    ) -> XllResult<()> {
        self.service.arm(generation, config)
    }

    pub(crate) fn disarm(&self, generation: RuntimeGeneration) -> XllResult<()> {
        self.service.disarm(generation)
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn set_ghost(&self, ghost: crate::shutdown_refinement::GhostHandle) {
        let _ = self.ghost.set(ghost.clone());
        self.service.with_published(|runtime| {
            if let Some(runtime) = runtime {
                runtime.set_ghost(ghost);
            }
        });
    }

    #[inline]
    pub(crate) fn is_none(&self) -> bool {
        self.service.is_none()
    }

    /// Acquire a read guard over the published `FormulaHandleService`.
    ///
    /// The warm path (runtime already initialized) performs a single
    /// `ArcSwap::load` with no `Mutex` and no `Arc::clone`.
    #[inline]
    pub(crate) fn read(&self) -> XllResult<FormulaHandleServiceRead> {
        self.service.read(
            |config| {
                FormulaHandleService::try_new_with_ingress(
                    usize::try_from(config.maximum_bindings())
                        .expect("handle capacity fits the platform usize"),
                    Some(crate::ingress::global_ingress()),
                )
                .map(Arc::new)
            },
            |_runtime| {
                #[cfg(any(test, feature = "shutdown-refinement"))]
                if let Some(ghost) = self.ghost.get() {
                    _runtime.set_ghost(Arc::clone(ghost));
                }
            },
        )
    }

    /// Owned `Arc` escape for test/benchmark code that needs to hold a
    /// `FormulaHandleService` beyond a call scope.
    #[cfg(any(test, feature = "bench-internals"))]
    pub(crate) fn get_owned(&self) -> XllResult<Arc<FormulaHandleService>> {
        let read = self.read()?;
        Ok(Arc::clone(read.as_arc()))
    }

    pub(crate) fn seal(
        &self,
        generation: Option<RuntimeGeneration>,
    ) -> XllResult<FormulaHandleServiceSealed> {
        self.service.seal(
            generation,
            crate::error::DiagnosticId::HANDLE_SLOT,
            || FormulaHandleServiceSealed::empty(generation),
            |handles| {
                let rtd_result = crate::rtd::shutdown(Arc::clone(&handles));
                let handle_result = handles.seal();
                rtd_result.and(handle_result).map(|registry| {
                    FormulaHandleServiceSealed::from_service(generation, handles, registry)
                })
            },
        )
    }
}

pub(crate) struct FormulaHandleServiceResolver<'call> {
    slot: &'call FormulaHandleServiceSlot,
    resolved: OnceCell<XllResult<FormulaHandleServiceRead>>,
}

impl<'call> FormulaHandleServiceResolver<'call> {
    #[inline]
    pub(crate) fn new(slot: &'call FormulaHandleServiceSlot) -> Self {
        Self {
            slot,
            resolved: OnceCell::new(),
        }
    }

    /// Returns a shared reference to the `FormulaHandleService`.
    ///
    /// The first call performs an `ArcSwap::load`; subsequent calls within the
    /// same UDF invocation return the cached guard with zero atomic operations.
    #[inline]
    pub(crate) fn get(&self) -> XllResult<&FormulaHandleService> {
        match self.resolved.get_or_init(|| self.slot.read()) {
            Ok(runtime) => Ok(runtime),
            Err(error) => Err(error.clone()),
        }
    }

    /// Returns a reference to the underlying `Arc` for paths that need
    /// ownership escape (RTD observation, `ensure_server`).
    #[inline]
    pub(crate) fn get_arc(&self) -> XllResult<&Arc<FormulaHandleService>> {
        match self.resolved.get_or_init(|| self.slot.read()) {
            Ok(runtime) => Ok(runtime.as_arc()),
            Err(error) => Err(error.clone()),
        }
    }
}
