use super::publication::{InsertedPublication, ObjectAllocation, PublicationReservation};
use super::registry::HandleRegistrySealed;
use super::{
    ExcelHandleObject, Handle, HandleAlias, HandlePrepareState,
    HandleRefinementHooks, HandleStore, HandleTopicKey, Initialization, InitializationPtr,
    ObjectBinding, PrepareDecision, PublishedTopic, PublishedTopicPtr, PublishedTopicState,
    TopicRemoval, TopicTable,
};
#[cfg(any(target_os = "windows", test))]
use super::{FormulaLifetimeGeneration, FormulaObserverId, HandleConnection};
use crate::generation::RuntimeGeneration;
use crate::generation::TopicGeneration;
use crate::{XllError, XllResult};
use parking_lot::{Condvar, Mutex};
#[cfg(feature = "handles")]
use std::cell::OnceCell;
use std::cell::RefCell;
use std::sync::atomic::AtomicBool;

pub(crate) struct NewObject(ObjectBinding);

impl NewObject {
    fn new(value: ObjectBinding) -> Self {
        Self(value)
    }

    pub(super) fn into_binding(self) -> ObjectBinding {
        self.0
    }
}

pub(crate) struct ExistingObject(ObjectBinding);

impl ExistingObject {
    fn new(object: ObjectBinding) -> Self {
        Self(object)
    }

    pub(super) fn into_binding(self) -> ObjectBinding {
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
    pub(crate) fn set_trace_sink(&self, trace: crate::shutdown_trace::ShutdownTraceHandle) {
        self.store.set_trace_sink(trace);
    }

    pub(super) fn refinement_token(&self, token: &str) -> super::TokenWire {
        self.store.refinement_token(token)
    }

    #[cfg(test)]
    pub(crate) fn refinement_trace_json(&self) -> String {
        self.refinement.trace_json()
    }

    #[cfg(all(target_os = "windows", any(test, feature = "refinement")))]
    pub(crate) fn lifetime_trace(&self) -> Option<crate::shutdown_trace::ShutdownTraceHandle> {
        self.store.trace_handle()
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
        lifetime_key: String,
        token: String,
        generation: TopicGeneration,
        observe: impl FnOnce(&str, &str) -> XllResult<()>,
    ) -> XllResult<HandlePreparation> {
        observe(&lifetime_key, &token)?;
        self.topics.is_current(key, generation, &token)?;
        Ok(HandlePreparation::Reused { token })
    }

    pub(super) fn commit_publication(
        &self,
        key: HandleTopicKey,
        generation: TopicGeneration,
        initialization: InitializationPtr,
        publication: PublishedTopicPtr,
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
                    object.into_object_binding()?,
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
                Initialization {
                    owner,
                    owner_done: AtomicBool::new(false),
                    wait: Mutex::new(()),
                    completed: Condvar::new(),
                    refinement_id,
                    #[cfg(test)]
                    waiters: std::sync::atomic::AtomicUsize::new(0),
                }
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
                lifetime_key,
                generation,
            } => {
                return self.observe_existing(
                    key,
                    lifetime_key,
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
            PublicationReservation::new(self, key, generation, initialization);
        let prepared = create()?;
        let InsertedPublication {
            transaction: publication_txn,
            token,
            allocation,
        } = publication_reservation.insert_object::<T>(prepared)?;
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
        let publication = PublishedTopic::new(token.clone(), key.format_lifetime_key());
        let publication_txn = publication_txn.publish_and_observe(
            publication,
            observe
                .take()
                .expect("the cold path owns the observation closure"),
            |publication| {
                self.refinement.observe_publish_and_install(
                    &key,
                    initialization.refinement_id,
                    self.refinement_token(&token),
                    &publication.lifetime_key,
                );
            },
        )?;
        publication_txn.commit()?;
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
        let Some(publication) = self.topics.published().load(&key) else {
            return Ok(None);
        };
        if publication.state() != PublishedTopicState::Live {
            return Ok(None);
        }

        let reader_id = self.refinement.observe_begin_warm_read(&key);
        let observed = observe
            .take()
            .expect("a live warm publication consumes the observation closure")(
            &publication.lifetime_key,
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
    pub(crate) fn claim_lifetime(
        &self,
        lifetime_key: &str,
        lifetime_generation: FormulaLifetimeGeneration,
    ) -> XllResult<()> {
        let _key = self
            .topics
            .claim_lifetime(lifetime_key, lifetime_generation)?;
        self.refinement
            .observe_claim_lifetime(&_key, lifetime_generation);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn connect(
        &self,
        lifetime_generation: FormulaLifetimeGeneration,
        topic_id: i32,
        lifetime_key: &str,
    ) -> XllResult<String> {
        let owner = FormulaObserverId {
            generation: lifetime_generation,
            topic_id,
        };
        let (key, token, created) =
            self.connect_inner(lifetime_generation, topic_id, lifetime_key)?;
        if created && let Err(error) = self.commit_connection(owner, key) {
            self.rollback_connection(owner, key);
            return Err(error);
        }
        Ok(token)
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn connect_transaction(
        &self,
        lifetime_generation: FormulaLifetimeGeneration,
        topic_id: i32,
        lifetime_key: &str,
    ) -> XllResult<HandleConnection<'_>> {
        let owner = FormulaObserverId {
            generation: lifetime_generation,
            topic_id,
        };
        let (key, token, created) =
            self.connect_inner(lifetime_generation, topic_id, lifetime_key)?;
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
        lifetime_generation: FormulaLifetimeGeneration,
        topic_id: i32,
        lifetime_key: &str,
    ) -> XllResult<(HandleTopicKey, String, bool)> {
        let owner = FormulaObserverId {
            generation: lifetime_generation,
            topic_id,
        };
        let (key, token, created) =
            self.topics
                .connect(lifetime_generation, owner, lifetime_key)?;
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
        owner: FormulaObserverId,
        key: HandleTopicKey,
    ) -> XllResult<()> {
        self.topics.commit_connection(owner, key)?;
        self.refinement.observe_commit_connection(&key, owner);
        Ok(())
    }

    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn rollback_connection(&self, owner: FormulaObserverId, key: HandleTopicKey) {
        if !self.topics.rollback_connection(owner, key) {
            return;
        }
        self.refinement.observe_rollback_connection(&key, owner);
    }

    #[cfg(test)]
    pub(crate) fn rollback(&self, lifetime_key: &str) {
        if let Some(removed) = self.topics.remove_by_lifetime_key(lifetime_key) {
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
    pub(crate) fn disconnect(&self, lifetime_generation: FormulaLifetimeGeneration, topic_id: i32) {
        let owner = FormulaObserverId {
            generation: lifetime_generation,
            topic_id,
        };
        let Some(removed) = self.topics.remove_by_observer(owner) else {
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

    pub(crate) fn seal(&self) -> XllResult<HandleRegistrySealed> {
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
    pub(crate) fn terminate_topics(&self, lifetime_generation: FormulaLifetimeGeneration) {
        let removals = self.topics.remove_generation(lifetime_generation);
        if !removals.is_empty() {
            self.refinement
                .observe_detach_generation(lifetime_generation);
        }
        for removed in removals {
            self.remove_topic_value(&removed, "handle lifetime termination");
        }
    }

    pub(crate) fn terminate_all_topics(&self) {
        let removals = self.topics.remove_all();
        for removed in removals {
            self.remove_topic_value(&removed, "handle lifetime termination");
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.store.len()
    }
}

impl super::lifetime::FormulaLifetimeBackend for FormulaHandleService {
    #[cfg(target_os = "windows")]
    fn identity(&self) -> usize {
        self as *const Self as usize
    }

    fn terminate_all_topics(&self) {
        FormulaHandleService::terminate_all_topics(self);
    }

    #[cfg(all(target_os = "windows", any(test, feature = "refinement")))]
    fn lifetime_trace(&self) -> Option<crate::shutdown_trace::ShutdownTraceHandle> {
        #[cfg(target_os = "windows")]
        {
            FormulaHandleService::lifetime_trace(self)
        }
        #[cfg(not(target_os = "windows"))]
        {
            None
        }
    }

    #[cfg(target_os = "windows")]
    fn claim_lifetime(
        &self,
        lifetime_key: &str,
        lifetime_generation: FormulaLifetimeGeneration,
    ) -> XllResult<()> {
        FormulaHandleService::claim_lifetime(self, lifetime_key, lifetime_generation)
    }

    #[cfg(target_os = "windows")]
    fn connect_lifetime<'a>(
        &'a self,
        lifetime_generation: FormulaLifetimeGeneration,
        topic_id: i32,
        lifetime_key: &str,
    ) -> XllResult<Box<dyn super::lifetime::FormulaLifetimeConnection + 'a>> {
        Ok(Box::new(FormulaHandleService::connect_transaction(
            self,
            lifetime_generation,
            topic_id,
            lifetime_key,
        )?))
    }

    #[cfg(target_os = "windows")]
    fn disconnect(&self, lifetime_generation: FormulaLifetimeGeneration, topic_id: i32) {
        FormulaHandleService::disconnect(self, lifetime_generation, topic_id);
    }

    #[cfg(target_os = "windows")]
    fn terminate_topics(&self, lifetime_generation: FormulaLifetimeGeneration) {
        FormulaHandleService::terminate_topics(self, lifetime_generation);
    }
}

/// The handle runtime has stopped accepting work and its registry has removed
/// every live binding. The service keeps the generation alive until add-in
/// state cleanup has completed and object/lease quiescence is certified.
enum FormulaHandleServiceSealed {
    Present {
        generation: RuntimeGeneration,
        service: Box<FormulaHandleService>,
        registry: HandleRegistrySealed,
    },
}

impl FormulaHandleServiceSealed {
    fn from_service(
        generation: Option<RuntimeGeneration>,
        service: Box<FormulaHandleService>,
        registry: super::registry::HandleRegistrySealed,
    ) -> Self {
        Self::Present {
            generation: generation.expect("a published formula handle service has a generation"),
            service,
            registry,
        }
    }
}

impl crate::shutdown::HandleStoreTeardown for FormulaHandleServiceSealed {
    fn finish(self: Box<Self>) -> XllResult<crate::shutdown::HandlesQuiescent> {
        match *self {
            Self::Present {
                generation,
                service,
                registry,
            } => service.store.quiescent(&registry, Some(generation)),
        }
    }
}

#[cfg(feature = "handles")]
pub(crate) struct FormulaHandleServiceSlot {
    service: xlfn_kernel::service_slot::GenerationServiceSlot<
        crate::addin::HandleConfig,
        FormulaHandleService,
        crate::XllError,
    >,
    #[cfg(any(test, feature = "refinement"))]
    trace: std::sync::OnceLock<crate::shutdown_trace::ShutdownTraceHandle>,
}

/// A non-owning read capability over the slot-owned `FormulaHandleService`.
/// The warm path acquires one drain permit and one atomic pointer load.
#[cfg(feature = "handles")]
pub(crate) type FormulaHandleServiceRead<'a> =
    xlfn_kernel::service_slot::GenerationServiceRead<'a, FormulaHandleService>;

#[cfg(feature = "handles")]
impl FormulaHandleServiceSlot {
    pub(crate) const fn new() -> Self {
        Self {
            service: xlfn_kernel::service_slot::GenerationServiceSlot::new(),
            #[cfg(any(test, feature = "refinement"))]
            trace: std::sync::OnceLock::new(),
        }
    }

    pub(crate) fn arm(&self, config: crate::addin::HandleConfig) -> XllResult<()> {
        self.service
            .arm(config)
            .map_err(crate::error::map_service_slot_error)
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
            .map_err(crate::error::map_service_slot_error)
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn set_trace_sink(&self, trace: crate::shutdown_trace::ShutdownTraceHandle) {
        let _ = self.trace.set(std::sync::Arc::clone(&trace));
        self.service.with_published(|runtime| {
            if let Some(runtime) = runtime {
                runtime.set_trace_sink(trace);
            }
        });
    }

    #[inline]
    pub(crate) fn is_none(&self) -> bool {
        self.service.is_none()
    }

    /// Acquire a read guard over the published `FormulaHandleService`.
    ///
    /// The warm path performs one admission increment and one pointer load.
    #[inline]
    pub(crate) fn read(&self) -> XllResult<FormulaHandleServiceRead<'_>> {
        self.service
            .read(
                |config| {
                    FormulaHandleService::try_new(
                        usize::try_from(config.maximum_bindings())
                            .expect("handle capacity fits the platform usize"),
                    )
                    .map(Box::new)
                },
                |_runtime| {
                    #[cfg(any(test, feature = "refinement"))]
                    if let Some(trace) = self.trace.get() {
                        _runtime.set_trace_sink(std::sync::Arc::clone(trace));
                    }
                },
            )
            .map_err(crate::error::map_service_slot_error)
    }

    /// Read an already-published service without initializing a cold slot.
    ///
    /// RTD shutdown uses this read-only probe from the generation service
    /// bundle. The handle slot itself remains independent of the RTD adapter.
    pub(crate) fn read_if_ready(&self) -> Option<FormulaHandleServiceRead<'_>> {
        self.service.read_if_ready()
    }

    pub(crate) fn seal(
        &self,
        generation: Option<RuntimeGeneration>,
    ) -> XllResult<crate::shutdown::HandlesSealed> {
        let sealed = self
            .service
            .seal(|| None, |handles| handles.seal().map(Some))
            .map_err(crate::error::map_service_slot_error)?;
        let (service, registry) = sealed.into_parts();
        match (service, registry) {
            (None, None) => Ok(crate::shutdown::HandlesSealed::empty(generation)),
            (Some(service), Some(registry)) => Ok(crate::shutdown::HandlesSealed::from_teardown(
                generation,
                FormulaHandleServiceSealed::from_service(generation, service, registry),
            )),
            _ => xlfn_kernel::invariant::fail_stop(),
        }
    }
}

#[cfg(feature = "handles")]
pub(crate) struct FormulaHandleServiceResolver<'call> {
    slot: &'call FormulaHandleServiceSlot,
    resolved: OnceCell<XllResult<FormulaHandleServiceRead<'call>>>,
}

#[cfg(feature = "handles")]
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
    /// The first call acquires a read permit; subsequent calls within the same
    /// UDF invocation return the cached guard without another admission.
    #[inline]
    pub(crate) fn get(&self) -> XllResult<&FormulaHandleService> {
        match self.resolved.get_or_init(|| self.slot.read()) {
            Ok(runtime) => Ok(runtime),
            Err(error) => Err(error.clone()),
        }
    }

}
