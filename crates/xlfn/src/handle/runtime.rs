use super::publication::{InsertedPublication, ObjectAllocation, PublicationReservation};
use super::{
    ExcelHandleObject, FormulaBinding, Handle, HandleAlias, HandlePrepareState,
    HandleRefinementHooks, HandleStore, HandleTopicKey, Initialization, PrepareDecision,
    PublishedTopic, PublishedTopicState, SharedObject, TopicRemoval, TopicTable,
};
#[cfg(any(target_os = "windows", test))]
use super::{HandleConnection, HandleTopicOwner};
use crate::generation::RuntimeGeneration;
#[cfg(any(target_os = "windows", test))]
use crate::generation::ServerGeneration;
use crate::generation::TopicGeneration;
use crate::runtime_components::GenerationServices;
use crate::shutdown::HandleRegistrySealed;
use crate::{XllError, XllResult};
use parking_lot::{Condvar, Mutex};
use std::cell::OnceCell;
use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

pub(crate) struct NewObject(SharedObject);

impl NewObject {
    fn new(value: SharedObject) -> Self {
        Self(value)
    }

    pub(super) fn into_shared(self) -> SharedObject {
        self.0
    }
}

pub(crate) struct ExistingObject(SharedObject);

impl ExistingObject {
    fn new(object: SharedObject) -> Self {
        Self(object)
    }

    pub(super) fn into_shared(self) -> SharedObject {
        self.0
    }
}

pub(crate) enum PreparedHandleObject {
    New(NewObject),
    Existing(ExistingObject),
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum HandlePreparation {
    Published { token: String },
    Reused { token: String },
}

impl HandlePreparation {
    pub(crate) fn into_token(self) -> String {
        match self {
            Self::Published { token } | Self::Reused { token } => token,
        }
    }

    #[cfg(test)]
    pub(crate) fn is_published(&self) -> bool {
        matches!(self, Self::Published { .. })
    }
}

thread_local! {
    static ACTIVE_HANDLE_INITIALIZATION_KEYS: RefCell<Vec<HandleTopicKey>> = const { RefCell::new(Vec::new()) };
}

pub(crate) struct HandleInitializationGuard {
    key: HandleTopicKey,
}

impl HandleInitializationGuard {
    pub(crate) fn enter(key: HandleTopicKey) -> XllResult<Self> {
        ACTIVE_HANDLE_INITIALIZATION_KEYS.with(|active| {
            let mut active = active.borrow_mut();
            if active.contains(&key) {
                return Err(XllError::ReentrantCall);
            }
            active.push(key);
            Ok(Self { key })
        })
    }
}

impl Drop for HandleInitializationGuard {
    fn drop(&mut self) {
        ACTIVE_HANDLE_INITIALIZATION_KEYS.with(|active| {
            let popped = active
                .borrow_mut()
                .pop()
                .expect("handle initialization stack remains balanced");
            debug_assert_eq!(popped, self.key);
        });
    }
}

/// Runtime-owned handle topics. Application code never inserts or removes
/// entries directly; generated UDF boundaries and Excel RTD callbacks do so.
pub(crate) struct FormulaHandleService {
    pub(super) store: HandleStore,
    pub(super) topics: TopicTable,
    pub(super) prepares: HandlePrepareState,
    pub(super) refinement: HandleRefinementHooks,
}

impl FormulaHandleService {
    pub(crate) fn try_new(maximum_bindings: usize) -> XllResult<Self> {
        let store = HandleStore::try_new(maximum_bindings)?;
        let registry_session = store.session();
        Ok(Self {
            store,
            topics: TopicTable::new(maximum_bindings),
            prepares: HandlePrepareState::new(),
            refinement: HandleRefinementHooks::new(registry_session),
        })
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn set_ghost(&self, ghost: crate::shutdown_refinement::GhostHandle) {
        self.store.set_ghost(ghost);
    }

    pub(super) fn refinement_token(&self, token: &str) -> super::TokenWire {
        self.store.refinement_token(token)
    }

    #[cfg(test)]
    pub(crate) fn refinement_trace_json(&self) -> String {
        self.refinement.trace_json()
    }

    #[cfg(all(target_os = "windows", any(test, feature = "refinement")))]
    pub(crate) fn rtd_ghost(&self) -> Option<crate::shutdown_refinement::GhostHandle> {
        self.store.ghost_handle()
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn new(maximum_bindings: usize) -> Self {
        Self::try_new(maximum_bindings).expect("test host provides an OS CSPRNG")
    }

    #[cfg(test)]
    pub(crate) fn prepare<T, K>(
        &self,
        key: K,
        create: impl FnOnce() -> XllResult<T>,
    ) -> XllResult<HandlePreparation>
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
    ) -> XllResult<HandlePreparation> {
        observe(&rtd_key, &token)?;
        self.topics.is_current(key, generation, &token)?;
        Ok(HandlePreparation::Reused { token })
    }

    pub(super) fn commit_publication(
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
    ) -> XllResult<HandlePreparation>
    where
        T: ExcelHandleObject,
        K: Into<HandleTopicKey>,
    {
        self.prepare_observed_object::<T, K>(
            key,
            || {
                create().map(|value| {
                    self.store
                        .erase(value)
                        .map(|value| PreparedHandleObject::New(NewObject::new(value)))
                })?
            },
            observe,
        )
    }

    pub(crate) fn prepare_observed_alias<T, K>(
        &self,
        key: K,
        object: HandleAlias<'_, T>,
        observe: impl FnOnce(&str, &str) -> XllResult<()>,
    ) -> XllResult<HandlePreparation>
    where
        T: ExcelHandleObject,
        K: Into<HandleTopicKey>,
    {
        self.prepare_observed_object::<T, K>(
            key,
            || {
                Ok(PreparedHandleObject::Existing(ExistingObject::new(
                    object.into_shared_object(),
                )))
            },
            observe,
        )
    }

    fn prepare_observed_object<T, K>(
        &self,
        key: K,
        create: impl FnOnce() -> XllResult<PreparedHandleObject>,
        observe: impl FnOnce(&str, &str) -> XllResult<()>,
    ) -> XllResult<HandlePreparation>
    where
        T: ExcelHandleObject,
        K: Into<HandleTopicKey>,
    {
        let key = key.into();
        let _active_initialization = HandleInitializationGuard::enter(key)?;
        let _prepare = self.prepares.try_enter().ok_or(XllError::Closing)?;
        let _refinement_prepare = self.refinement.observe_prepare();
        let mut observe = Some(observe);
        if let Some(preparation) = self.prepare_warm(key, &mut observe)? {
            return Ok(preparation);
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
                return self.observe_existing(
                    key,
                    rtd_key,
                    token,
                    generation,
                    observe
                        .take()
                        .expect("the observation closure is still owned"),
                );
            }

            PrepareDecision::Initialize {
                initialization,
                generation,
            } => (initialization, generation),

            PrepareDecision::Wait { .. } => unreachable!("wait decisions never leave the loop"),
        };

        //
        // Cold path: no existing topic, invoke the factory.
        //
        let publication_reservation =
            PublicationReservation::new(self, key, generation, Arc::clone(&initialization));
        let prepared = create()?;
        let InsertedPublication {
            transaction: publication_txn,
            token,
            binding_id,
            object_id,
            allocation,
        } = publication_reservation.insert_object::<T>(prepared)?;
        let binding = FormulaBinding {
            id: binding_id,
            object_id,
        };
        let parsed = self.refinement_token(&token);
        if allocation == ObjectAllocation::Reused {
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
        let rtd_key: Arc<str> = key.format_rtd_key().into();
        let publication = triomphe::Arc::new(PublishedTopic::new(
            binding,
            token.clone(),
            Arc::clone(&rtd_key),
        ));
        let publication_txn = publication_txn.publish_and_observe(
            publication.clone(),
            Arc::clone(&rtd_key),
            observe
                .take()
                .expect("the cold path owns the observation closure"),
            || {
                self.refinement.observe_publish_and_install(
                    &key,
                    initialization.refinement_id,
                    self.refinement_token(&token),
                    &rtd_key,
                );
            },
        )?;
        publication_txn.commit(&publication)?;
        Ok(HandlePreparation::Published { token })
    }

    /// Attempts the warm publication path and leaves the observation closure
    /// untouched when the topic is not live. The cold path then owns the same
    /// closure and the same call-scoped preparation admission.
    fn prepare_warm<F>(
        &self,
        key: HandleTopicKey,
        observe: &mut Option<F>,
    ) -> XllResult<Option<HandlePreparation>>
    where
        F: FnOnce(&str, &str) -> XllResult<()>,
    {
        let published = self.topics.published().load(&key);
        let Some(publication) = published.get(&key) else {
            return Ok(None);
        };
        if publication.state() != PublishedTopicState::Live {
            return Ok(None);
        }

        let reader_id = self.refinement.observe_begin_warm_read(&key);
        let observed = observe
            .take()
            .expect("a live warm publication consumes the observation closure")(
            &publication.rtd_key,
            &publication.token,
        );
        if let Err(error) = observed {
            match publication.state() {
                PublishedTopicState::Live => self.refinement.observe_fail_warm_read(reader_id),
                PublishedTopicState::Stale | PublishedTopicState::Closing => {
                    self.refinement.observe_abandon_warm_read(reader_id)
                }
                PublishedTopicState::Provisional => {}
            }
            return Err(error);
        }

        match publication.state() {
            PublishedTopicState::Live => {
                self.refinement.observe_finish_warm_read(reader_id);
                Ok(Some(HandlePreparation::Reused {
                    token: publication.token.clone(),
                }))
            }
            PublishedTopicState::Closing => {
                self.refinement.observe_abandon_warm_read(reader_id);
                Err(XllError::Closing)
            }
            PublishedTopicState::Provisional | PublishedTopicState::Stale => {
                self.refinement.observe_abandon_warm_read(reader_id);
                Err(XllError::StaleHandle)
            }
        }
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn claim_server(
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
    pub(crate) fn connect(
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
    pub(crate) fn rollback(&self, rtd_key: &str) {
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
    pub(crate) fn disconnect(&self, server_generation: ServerGeneration, excel_topic_id: i32) {
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

    pub(crate) fn lookup<'call, T>(
        &self,
        scope: &'call crate::call::CallScope<'call>,
        token: &str,
    ) -> XllResult<Handle<'call, T>>
    where
        T: ExcelHandleObject,
    {
        self.store.lookup(scope, token)
    }

    pub(crate) fn seal(&self) -> XllResult<crate::shutdown::HandleRegistrySealed> {
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
    pub(crate) fn terminate_topics(&self, server_generation: ServerGeneration) {
        let removals = self.topics.remove_generation(server_generation);
        if !removals.is_empty() {
            self.refinement.observe_detach_generation(server_generation);
        }
        for removed in removals {
            self.remove_topic_value(&removed, "handle RTD termination");
        }
    }

    pub(crate) fn terminate_all_topics(&self) {
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

/// The handle runtime has stopped accepting work and its registry has removed
/// every live binding. The service keeps the generation alive until add-in
/// state cleanup has completed and object/lease quiescence is certified.
pub(crate) enum FormulaHandleServiceSealed {
    Absent {
        generation: Option<RuntimeGeneration>,
    },
    Present {
        generation: RuntimeGeneration,
        service: Arc<FormulaHandleService>,
        registry: HandleRegistrySealed,
    },
}

/// Proof that the handle registry for one specific runtime generation has
/// completed its object/lease quiescence check. The generation identity
/// travels with the proof so a certificate cannot be silently reused for a
/// different service instance.
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
    pub(crate) fn empty(generation: Option<RuntimeGeneration>) -> Self {
        Self::Absent { generation }
    }

    fn from_service(
        generation: Option<RuntimeGeneration>,
        service: Arc<FormulaHandleService>,
        registry: crate::shutdown::HandleRegistrySealed,
    ) -> Self {
        Self::Present {
            generation: generation.expect("a published formula handle service has a generation"),
            service,
            registry,
        }
    }

    pub(crate) fn finish(self) -> XllResult<HandleStoreQuiescent> {
        match self {
            Self::Absent { generation } => Ok(HandleStoreQuiescent::new(generation)),
            Self::Present {
                generation,
                service,
                registry,
            } => service.store.quiescent(&registry, Some(generation)),
        }
    }
}

pub(crate) struct FormulaHandleServiceSlot {
    service: xlfn_kernel::service_slot::GenerationServiceSlot<
        crate::addin::HandleConfig,
        FormulaHandleService,
        crate::XllError,
    >,
    #[cfg(any(test, feature = "refinement"))]
    ghost: std::sync::OnceLock<crate::shutdown_refinement::GhostHandle>,
}

/// A read capability that holds an `arc_swap::Guard` over a published
/// `FormulaHandleService`.  The warm path acquires this without any `Mutex` or
/// `Arc::clone`.
pub(crate) type FormulaHandleServiceRead =
    xlfn_kernel::service_slot::GenerationServiceRead<FormulaHandleService>;

impl FormulaHandleServiceSlot {
    pub(crate) const fn new() -> Self {
        Self {
            service: xlfn_kernel::service_slot::GenerationServiceSlot::new(),
            #[cfg(any(test, feature = "refinement"))]
            ghost: std::sync::OnceLock::new(),
        }
    }

    pub(crate) fn arm(&self, config: crate::addin::HandleConfig) -> XllResult<()> {
        self.service
            .arm(config)
            .map_err(crate::runtime_components::map_service_error)
    }

    /// Construct and publish the handle service as part of generation open.
    ///
    /// Handle service construction is deterministic from `HandleConfig` and
    /// does not depend on a first UDF call.  Keeping the initialization at the
    /// open boundary makes a published generation a usable handle generation;
    /// the generic slot remains lazy for services whose construction is truly
    /// demand-driven.
    pub(crate) fn initialize(&self) -> XllResult<()> {
        let _read = self.read()?;
        Ok(())
    }

    pub(crate) fn disarm(&self) -> XllResult<()> {
        self.service
            .disarm()
            .map_err(crate::runtime_components::map_service_error)
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn set_ghost(&self, ghost: crate::shutdown_refinement::GhostHandle) {
        let _ = self.ghost.set(std::sync::Arc::clone(&ghost));
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
        self.service
            .read(
                |config| {
                    FormulaHandleService::try_new(
                        usize::try_from(config.maximum_bindings())
                            .expect("handle capacity fits the platform usize"),
                    )
                    .map(Arc::new)
                },
                |_runtime| {
                    #[cfg(any(test, feature = "refinement"))]
                    if let Some(ghost) = self.ghost.get() {
                        _runtime.set_ghost(Arc::clone(ghost));
                    }
                },
            )
            .map_err(crate::runtime_components::map_service_error)
    }

    /// Read an already-published service without initializing a cold slot.
    ///
    /// RTD shutdown uses this read-only probe from the generation service
    /// bundle. The handle slot itself remains independent of the RTD adapter.
    pub(crate) fn read_if_ready(&self) -> Option<FormulaHandleServiceRead> {
        self.service.read_if_ready()
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
        self.service
            .seal(
                crate::XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::HANDLE_SLOT,
                },
                || FormulaHandleServiceSealed::empty(generation),
                |handles| {
                    let handle_result = handles.seal();
                    handle_result.map(|registry| {
                        FormulaHandleServiceSealed::from_service(generation, handles, registry)
                    })
                },
            )
            .map_err(crate::runtime_components::map_service_error)
    }
}

pub(crate) struct FormulaHandleServiceResolver<'call> {
    services: &'call GenerationServices,
    _call: std::marker::PhantomData<&'call ()>,
    resolved: OnceCell<XllResult<FormulaHandleServiceRead>>,
}

impl<'call> FormulaHandleServiceResolver<'call> {
    #[inline]
    pub(crate) fn new(services: &'call GenerationServices) -> Self {
        Self {
            services,
            _call: std::marker::PhantomData,
            resolved: OnceCell::new(),
        }
    }

    #[inline]
    pub(crate) fn services(&self) -> &'call GenerationServices {
        self.services
    }

    /// Returns a shared reference to the `FormulaHandleService`.
    ///
    /// The first call performs an `ArcSwap::load`; subsequent calls within the
    /// same UDF invocation return the cached guard with zero atomic operations.
    #[inline]
    pub(crate) fn get(&self) -> XllResult<&FormulaHandleService> {
        match self
            .resolved
            .get_or_init(|| self.services.formula_handle_slot().read())
        {
            Ok(runtime) => Ok(runtime),
            Err(error) => Err(error.clone()),
        }
    }

    /// Returns a reference to the underlying `Arc` for paths that need
    /// ownership escape (RTD observation, `ensure_server`).
    #[inline]
    pub(crate) fn get_arc(&self) -> XllResult<&Arc<FormulaHandleService>> {
        match self
            .resolved
            .get_or_init(|| self.services.formula_handle_slot().read())
        {
            Ok(runtime) => Ok(runtime.as_arc()),
            Err(error) => Err(error.clone()),
        }
    }
}
