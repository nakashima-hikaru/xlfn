use crate::generation::{OpenAttemptId, OpeningGeneration, RemovalEpoch};
use crate::host_callback::HostCallbackSession;
use crate::lifecycle::LifecycleAccess;
#[cfg(test)]
use crate::lifecycle::OpenFailureDisposition;
use crate::registration::{HostMutationJournal, RegistrationId};
use crate::runtime::AddinLifecycleAccess;
use crate::runtime::capabilities::OpenDeps;
use crate::runtime_components::GenerationServices;
use crate::{XllError, XllResult};
use xlfn_kernel::thread_affine::ThreadAffineError;

/// A state payload in the open transaction protocol.
pub(crate) trait OpeningState<A: crate::Addin>: Sized {
    fn attempt_id(&self) -> OpenAttemptId;
    fn abandon(self, deps: &OpenDeps<'_, A>);
}

type ServiceInputs<A> = crate::runtime_components::GenerationServiceInputs<
    crate::addin::AddinSubscription<A>,
    crate::addin::AddinSources<A>,
>;
type InitializedOpenCore<A> = OpenCore<A, ServiceInputs<A>>;
type RollbackOpenCore<A> = OpenCore<A, Option<ServiceInputs<A>>>;

/// Service inputs are absent before initialization and mandatory afterwards.
/// Only the rollback projection erases that distinction into an `Option`.
pub(crate) struct OpenCore<A: crate::Addin, I = ()> {
    attempt_id: OpenAttemptId,
    module_opening: crate::module_runtime::ModuleOpening,
    service_inputs: I,
    _addin: std::marker::PhantomData<fn() -> A>,
}

impl<A: crate::Addin> OpenCore<A> {
    fn new(
        attempt_id: OpenAttemptId,
        module_opening: crate::module_runtime::ModuleOpening,
    ) -> Self {
        Self {
            attempt_id,
            module_opening,
            service_inputs: (),
            _addin: std::marker::PhantomData,
        }
    }

    fn initialized(self, inputs: ServiceInputs<A>) -> InitializedOpenCore<A> {
        self.map_inputs(|()| inputs)
    }

    fn into_rollback(self) -> RollbackOpenCore<A> {
        self.map_inputs(|()| None)
    }
}

impl<A: crate::Addin> InitializedOpenCore<A> {
    fn into_rollback(self) -> RollbackOpenCore<A> {
        self.map_inputs(Some)
    }
}

impl<A: crate::Addin, I> OpenCore<A, I> {
    fn attempt_id(&self) -> OpenAttemptId {
        self.attempt_id
    }

    fn map_inputs<J>(self, map: impl FnOnce(I) -> J) -> OpenCore<A, J> {
        OpenCore {
            attempt_id: self.attempt_id,
            module_opening: self.module_opening,
            service_inputs: map(self.service_inputs),
            _addin: std::marker::PhantomData,
        }
    }

    pub(crate) fn into_parts(self) -> (OpenAttemptId, crate::module_runtime::ModuleOpening, I) {
        (self.attempt_id, self.module_opening, self.service_inputs)
    }
}

/// The open protocol has begun, but no host callback session is attached yet.
pub(crate) struct Begun<A: crate::Addin> {
    core: OpenCore<A>,
}

/// Host callback and registration journal ownership is now attached.
pub(crate) struct HostAttached<A: crate::Addin> {
    core: OpenCore<A>,
    host: HostOpeningState,
}

/// Application initialization has begun, but no state is available to quiesce.
/// This state deliberately cannot implement `RollbackState`: an unsuccessful
/// `Addin::open` may leave execution sources that only the application knows.
pub(crate) struct Initializing<A: crate::Addin> {
    core: OpenCore<A>,
    host: HostOpeningState,
}

/// `Addin::open` succeeded and its thread-affine lifecycle state is owned by
/// the transaction.
pub(crate) struct Initialized<A: crate::Addin> {
    core: InitializedOpenCore<A>,
    host: HostOpeningState,
    lifecycle: A::LifecycleState,
}

/// The generation is staged in canonical lifecycle state, while the
/// thread-affine lifecycle payload remains owned by the transaction.
pub(crate) struct GenerationStaged<A: crate::Addin> {
    core: InitializedOpenCore<A>,
    host: HostOpeningState,
    lifecycle: A::LifecycleState,
}

/// The lifecycle payload has been transferred to the runtime's thread-affine
/// slot. The transaction still owns the module and host capabilities.
pub(crate) struct LifecycleInstalled<A: crate::Addin> {
    core: InitializedOpenCore<A>,
    host: HostOpeningState,
}

/// All mandatory open steps are complete and the transaction may commit.
pub(crate) struct HostMutated<A: crate::Addin> {
    core: InitializedOpenCore<A>,
    host: HostOpeningState,
}

pub(crate) struct HostOpeningState {
    callbacks: HostCallbackSession,
    journal: HostMutationJournal,
}

impl HostOpeningState {
    fn new(callbacks: HostCallbackSession) -> Self {
        Self {
            callbacks,
            journal: HostMutationJournal::default(),
        }
    }

    pub(crate) fn into_parts(self) -> (HostCallbackSession, HostMutationJournal) {
        (self.callbacks, self.journal)
    }

    fn stage_registrations(&mut self, registrations: Vec<RegistrationId>) {
        self.journal.pending_registrations = registrations
            .into_iter()
            .map(crate::registration::PendingRegistration::from)
            .collect();
    }
}

/// Lifecycle ownership while an opening transaction is being rolled back.
/// The enum is a recovery projection, not an independent state machine: the
/// production transaction states above determine which variant can be made.
pub(crate) enum LifecycleOwnership<A: crate::Addin> {
    Owned(A::LifecycleState),
    Installed,
}

pub(crate) struct RollbackParts<A: crate::Addin> {
    core: RollbackOpenCore<A>,
    host: HostOpeningState,
    lifecycle: LifecycleOwnership<A>,
}

/// States that still own the host-side rollback session.
pub(crate) trait RollbackState<A: crate::Addin>: OpeningState<A> {
    fn into_rollback_parts(self) -> RollbackParts<A>;
}

fn forget_untrusted_lifecycle_state<A: crate::Addin>(state: A::LifecycleState) {
    #[allow(
        clippy::mem_forget,
        reason = "an abandoned open transaction must not run untrusted lifecycle destruction"
    )]
    std::mem::forget(state);
}

fn abandon_open<A: crate::Addin, I>(
    deps: &OpenDeps<'_, A>,
    core: OpenCore<A, I>,
    lifecycle: LifecycleOwnership<A>,
) {
    let (_, module_opening, _service_inputs) = core.into_parts();
    deps.lifecycle_control()
        .complete_open_abort(module_opening.rollback(|| {}));
    let control_api = deps.lifecycle_control();
    let mut control = control_api.access();
    control_api.quarantine_state(&mut control);
    drop(control);
    control_api.notify_all();
    if let LifecycleOwnership::Owned(state) = lifecycle {
        forget_untrusted_lifecycle_state::<A>(state);
    }
}

impl<A: crate::Addin> OpeningState<A> for Begun<A> {
    fn attempt_id(&self) -> OpenAttemptId {
        self.core.attempt_id()
    }

    fn abandon(self, deps: &OpenDeps<'_, A>) {
        let Self { core } = self;
        abandon_open(deps, core, LifecycleOwnership::Installed);
    }
}

impl<A: crate::Addin> OpeningState<A> for HostAttached<A> {
    fn attempt_id(&self) -> OpenAttemptId {
        self.core.attempt_id()
    }

    fn abandon(self, deps: &OpenDeps<'_, A>) {
        let Self { core, host: _ } = self;
        abandon_open(deps, core, LifecycleOwnership::Installed);
    }
}

impl<A: crate::Addin> OpeningState<A> for Initializing<A> {
    fn attempt_id(&self) -> OpenAttemptId {
        self.core.attempt_id()
    }

    fn abandon(self, deps: &OpenDeps<'_, A>) {
        let Self { core, host: _ } = self;
        let attempt = core.attempt_id();
        let reason = if std::thread::panicking() {
            crate::shutdown_trace::ShutdownFailure::BoundaryPanic
        } else {
            crate::shutdown_trace::ShutdownFailure::AddinShutdownFailed
        };
        abandon_open(deps, core, LifecycleOwnership::Installed);
        deps.observer().quarantine_open(attempt, reason);
    }
}

impl<A: crate::Addin> OpeningState<A> for Initialized<A> {
    fn attempt_id(&self) -> OpenAttemptId {
        self.core.attempt_id()
    }

    fn abandon(self, deps: &OpenDeps<'_, A>) {
        let Self {
            core,
            host: _,
            lifecycle,
        } = self;
        abandon_open(deps, core, LifecycleOwnership::Owned(lifecycle));
    }
}

impl<A: crate::Addin> OpeningState<A> for GenerationStaged<A> {
    fn attempt_id(&self) -> OpenAttemptId {
        self.core.attempt_id()
    }

    fn abandon(self, deps: &OpenDeps<'_, A>) {
        let Self {
            core,
            host: _,
            lifecycle,
        } = self;
        abandon_open(deps, core, LifecycleOwnership::Owned(lifecycle));
    }
}

impl<A: crate::Addin> OpeningState<A> for LifecycleInstalled<A> {
    fn attempt_id(&self) -> OpenAttemptId {
        self.core.attempt_id()
    }

    fn abandon(self, deps: &OpenDeps<'_, A>) {
        let Self { core, host: _ } = self;
        abandon_open(deps, core, LifecycleOwnership::Installed);
    }
}

impl<A: crate::Addin> OpeningState<A> for HostMutated<A> {
    fn attempt_id(&self) -> OpenAttemptId {
        self.core.attempt_id()
    }

    fn abandon(self, deps: &OpenDeps<'_, A>) {
        let Self { core, host: _ } = self;
        abandon_open(deps, core, LifecycleOwnership::Installed);
    }
}

impl<A: crate::Addin> RollbackState<A> for HostAttached<A> {
    fn into_rollback_parts(self) -> RollbackParts<A> {
        let Self { core, host } = self;
        RollbackParts {
            core: core.into_rollback(),
            host,
            lifecycle: LifecycleOwnership::Installed,
        }
    }
}

impl<A: crate::Addin> RollbackState<A> for Initialized<A> {
    fn into_rollback_parts(self) -> RollbackParts<A> {
        let Self {
            core,
            host,
            lifecycle,
        } = self;
        RollbackParts {
            core: core.into_rollback(),
            host,
            lifecycle: LifecycleOwnership::Owned(lifecycle),
        }
    }
}

impl<A: crate::Addin> RollbackState<A> for GenerationStaged<A> {
    fn into_rollback_parts(self) -> RollbackParts<A> {
        let Self {
            core,
            host,
            lifecycle,
        } = self;
        RollbackParts {
            core: core.into_rollback(),
            host,
            lifecycle: LifecycleOwnership::Owned(lifecycle),
        }
    }
}

impl<A: crate::Addin> RollbackState<A> for LifecycleInstalled<A> {
    fn into_rollback_parts(self) -> RollbackParts<A> {
        let Self { core, host } = self;
        RollbackParts {
            core: core.into_rollback(),
            host,
            lifecycle: LifecycleOwnership::Installed,
        }
    }
}

impl<A: crate::Addin> RollbackState<A> for HostMutated<A> {
    fn into_rollback_parts(self) -> RollbackParts<A> {
        let Self { core, host } = self;
        RollbackParts {
            core: core.into_rollback(),
            host,
            lifecycle: LifecycleOwnership::Installed,
        }
    }
}

pub(crate) trait HasHost {
    fn host_mut(&mut self) -> &mut HostOpeningState;
}

impl<A: crate::Addin> HasHost for HostAttached<A> {
    fn host_mut(&mut self) -> &mut HostOpeningState {
        &mut self.host
    }
}

impl<A: crate::Addin> HasHost for Initialized<A> {
    fn host_mut(&mut self) -> &mut HostOpeningState {
        &mut self.host
    }
}

impl<A: crate::Addin> HasHost for GenerationStaged<A> {
    fn host_mut(&mut self) -> &mut HostOpeningState {
        &mut self.host
    }
}

impl<A: crate::Addin> HasHost for LifecycleInstalled<A> {
    fn host_mut(&mut self) -> &mut HostOpeningState {
        &mut self.host
    }
}

impl<A: crate::Addin> HasHost for HostMutated<A> {
    fn host_mut(&mut self) -> &mut HostOpeningState {
        &mut self.host
    }
}

/// Owns the canonical lock until acquisition has either issued a transaction or
/// restored terminal quarantine. No independent attempt/module pair escapes.
struct OpenAcquisition<'runtime, A: crate::Addin> {
    deps: OpenDeps<'runtime, A>,
    control: Option<LifecycleAccess<'runtime, A>>,
    module: Option<crate::module_runtime::ModuleOpening>,
}

impl<A: crate::Addin> Drop for OpenAcquisition<'_, A> {
    fn drop(&mut self) {
        let Some(control) = self.control.as_mut() else {
            return;
        };
        self.deps.returns().close_admission();
        let lifecycle = self.deps.lifecycle_control();
        if let Some(module) = self.module.take() {
            lifecycle.complete_open_abort_locked(control, module.rollback(|| {}));
        }
        lifecycle.quarantine_state(control);
        lifecycle.notify_all();
    }
}

/// Rollback retains its issuer and responsibility across lifecycle installation,
/// failure publication and the complete cleanup operation.
pub(crate) struct OpenRollbackOwner<'runtime, A: crate::Addin> {
    deps: OpenDeps<'runtime, A>,
    core: Option<RollbackOpenCore<A>>,
    host: Option<HostOpeningState>,
    lifecycle: Option<LifecycleOwnership<A>>,
    armed: bool,
}

impl<A: crate::Addin> Drop for OpenRollbackOwner<'_, A> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Some(host) = self.host.take() {
            let (_, journal) = host.into_parts();
            self.deps.merge_host(journal);
        }
        let ownership = self
            .lifecycle
            .take()
            .unwrap_or(LifecycleOwnership::Installed);
        if let Some(core) = self.core.take() {
            abandon_open(&self.deps, core, ownership);
        } else {
            let lifecycle = self.deps.lifecycle_control();
            let mut control = lifecycle.access();
            self.deps.returns().close_admission();
            lifecycle.quarantine_state(&mut control);
            lifecycle.notify_all();
            if let LifecycleOwnership::Owned(state) = ownership {
                forget_untrusted_lifecycle_state::<A>(state);
            }
        }
    }
}

impl<A: crate::Addin> OpenRollbackOwner<'_, A> {
    pub(crate) fn run(mut self, lifecycle: &AddinLifecycleAccess<'_, A>) {
        use crate::boundary::report_boundary_error;
        let recovery = self.deps.recovery();
        if let Some(LifecycleOwnership::Owned(state)) = self.lifecycle.take()
            && let Err(error) = self.deps.install_addin_lifecycle(lifecycle, state)
        {
            let (state, reason) = error.into_parts();
            self.lifecycle = Some(LifecycleOwnership::Owned(state));
            report_boundary_error(
                "xlAutoOpen rollback lifecycle installation",
                &crate::lifecycle::lifecycle_access_error(reason),
            );
            drop(self);
            recovery.quarantine(None);
            return;
        }
        let (mut callbacks, journal) = self
            .host
            .take()
            .expect("rollback owns host session")
            .into_parts();
        self.deps.merge_host(journal);
        let attempt = self.core.as_ref().unwrap().attempt_id();
        let generation = Some(attempt.into_runtime_generation());
        let disposition =
            super::orchestration::LifecycleOrchestrator::new(self.deps).mark_open_failed(attempt);
        let (_, module, _service_inputs) = self.core.take().unwrap().into_parts();
        self.deps
            .lifecycle_control()
            .complete_open_abort(module.rollback(|| {}));
        #[cfg(test)]
        check_rollback_fault();
        if disposition.requires_rollback() {
            match crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(|| {
                recovery.rollback(lifecycle, &mut callbacks, generation)
            })) {
                Ok(outcome) if outcome.is_finalized() || outcome.cleanup_owned_elsewhere() => {}
                Ok(outcome) => {
                    let error = XllError::Internal {
                        diagnostic_id: crate::diagnostics::id::DiagnosticId::OPEN_ROLLBACK_FAILURE,
                    };
                    report_boundary_error("xlAutoOpen rollback", &error);
                    if let Some(hazard) = outcome.failure_hazard() {
                        recovery.quarantine(Some(hazard));
                    }
                }
                Err(_) => {
                    report_boundary_error("xlAutoOpen rollback", &XllError::Panic);
                    recovery.quarantine(None);
                }
            }
        }
        self.armed = false;
    }
}

#[cfg(test)]
std::thread_local! {
    static FAIL_OPEN_ACQUIRE: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
    static PANIC_ROLLBACK_TRANSFER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
#[cfg(test)]
fn check_open_acquire_fault(point: u8) -> XllResult<()> {
    FAIL_OPEN_ACQUIRE.with(|fault| {
        if fault.get() == point {
            fault.set(0);
            Err(XllError::Closing)
        } else {
            Ok(())
        }
    })
}
#[cfg(test)]
fn check_rollback_fault() {
    assert!(
        !PANIC_ROLLBACK_TRANSFER.with(|fault| fault.replace(false)),
        "injected rollback transfer panic"
    );
}

pub(crate) struct OpeningTxn<'runtime, A: crate::Addin, S: OpeningState<A>> {
    deps: OpenDeps<'runtime, A>,
    state: Option<S>,
}

type OpeningStageFailure<'runtime, A> = (
    XllError,
    Box<OpeningTxn<'runtime, A, Initialized<A>>>,
    Box<OpeningGeneration<A>>,
);

type LifecycleInstallFailure<'runtime, A> = (
    ThreadAffineError,
    Box<OpeningTxn<'runtime, A, GenerationStaged<A>>>,
);

impl<'runtime, A: crate::Addin, S: OpeningState<A>> OpeningTxn<'runtime, A, S> {
    fn new_state(deps: OpenDeps<'runtime, A>, state: S) -> Self {
        Self {
            deps,
            state: Some(state),
        }
    }

    fn take_state(&mut self) -> S {
        self.state
            .take()
            .expect("opening transaction state already consumed")
    }

    pub(crate) fn attempt_id(&self) -> OpenAttemptId {
        match &self.state {
            Some(state) => state.attempt_id(),
            None => panic!("opening transaction state already consumed"),
        }
    }

    pub(crate) fn deps(&self) -> OpenDeps<'runtime, A> {
        self.deps
    }

    pub(crate) fn into_rollback(mut self) -> OpenRollbackOwner<'runtime, A>
    where
        S: RollbackState<A>,
    {
        let parts = self.take_state().into_rollback_parts();
        OpenRollbackOwner {
            deps: self.deps,
            core: Some(parts.core),
            host: Some(parts.host),
            lifecycle: Some(parts.lifecycle),
            armed: true,
        }
    }
}

impl<'runtime, A: crate::Addin> OpeningTxn<'runtime, A, Begun<A>> {
    pub(crate) fn acquire(
        deps: OpenDeps<'runtime, A>,
        expected_removal_epoch: RemovalEpoch,
    ) -> XllResult<Self> {
        #[cfg(test)]
        let test_module_lease = crate::ingress::acquire_test_module_lease();
        let lifecycle = crate::lifecycle::LifecycleControl::new(deps.lifecycle());
        let mut control = lifecycle.access();
        let attempt = lifecycle.begin_open_state(&mut control, expected_removal_epoch)?;
        // From the first successful canonical mutation, this guard owns both
        // the original lock and the responsibility for an incomplete start.
        let mut acquisition = OpenAcquisition {
            deps,
            control: Some(control),
            module: None,
        };
        #[cfg(test)]
        check_open_acquire_fault(1)?;
        deps.returns().reopen_admission()?;
        acquisition.module = Some(crate::module_runtime::begin_open());
        #[cfg(test)]
        {
            *deps.lifecycle().test_module_lease.lock() = Some(test_module_lease);
        }
        #[cfg(test)]
        check_open_acquire_fault(2)?;
        deps.observer().begin_open(
            acquisition.control.as_ref().unwrap(),
            deps.returns(),
            expected_removal_epoch.get(),
            attempt,
        );
        let module = acquisition
            .module
            .take()
            .expect("acquisition owns its module");
        let transaction = Self::new_state(
            deps,
            Begun {
                core: OpenCore::new(attempt, module),
            },
        );
        // No fallible operation separates responsibility transfer from unlock.
        acquisition.control.take();
        Ok(transaction)
    }

    pub(crate) fn attach_host(
        self,
        callbacks: HostCallbackSession,
    ) -> OpeningTxn<'runtime, A, HostAttached<A>> {
        let mut transaction = self;
        let Begun { core } = transaction.take_state();
        OpeningTxn::new_state(
            transaction.deps,
            HostAttached {
                core,
                host: HostOpeningState::new(callbacks),
            },
        )
    }

    #[cfg(test)]
    pub(crate) fn fail_for_test(self) -> OpenFailureDisposition {
        let mut transaction = self;
        let attempt_id = transaction.attempt_id();
        let disposition =
            crate::runtime::orchestration::LifecycleOrchestrator::new(transaction.deps)
                .mark_open_failed(attempt_id);
        let Begun { core } = transaction.take_state();
        let (_, module_opening, _service_inputs) = core.into_parts();
        transaction
            .deps
            .lifecycle_control()
            .complete_open_abort(module_opening.rollback(|| {}));
        disposition
    }
}

impl<'runtime, A: crate::Addin> OpeningTxn<'runtime, A, HostAttached<A>> {
    pub(crate) fn begin_initialization(self) -> OpeningTxn<'runtime, A, Initializing<A>> {
        let mut transaction = self;
        let HostAttached { core, host } = transaction.take_state();
        OpeningTxn::new_state(transaction.deps, Initializing { core, host })
    }
}

impl<'runtime, A: crate::Addin> OpeningTxn<'runtime, A, Initializing<A>> {
    pub(crate) fn initialized(
        self,
        lifecycle: A::LifecycleState,
        service_inputs: crate::runtime_components::GenerationServiceInputs<
            crate::addin::AddinSubscription<A>,
            crate::addin::AddinSources<A>,
        >,
    ) -> OpeningTxn<'runtime, A, Initialized<A>> {
        let mut transaction = self;
        let Initializing { core, host } = transaction.take_state();
        let core = core.initialized(service_inputs);
        OpeningTxn::new_state(
            transaction.deps,
            Initialized {
                core,
                host,
                lifecycle,
            },
        )
    }
}

impl<'runtime, A: crate::Addin> OpeningTxn<'runtime, A, Initialized<A>> {
    pub(crate) fn stage_opening_generation(
        self,
        opening: OpeningGeneration<A>,
    ) -> Result<OpeningTxn<'runtime, A, GenerationStaged<A>>, OpeningStageFailure<'runtime, A>>
    {
        let mut transaction = self;
        let result = transaction
            .deps
            .lifecycle_control()
            .stage_opening_generation(transaction.attempt_id(), opening);
        match result {
            Ok(()) => {
                let Initialized {
                    core,
                    host,
                    lifecycle,
                } = transaction.take_state();
                Ok(OpeningTxn::new_state(
                    transaction.deps,
                    GenerationStaged {
                        core,
                        host,
                        lifecycle,
                    },
                ))
            }
            Err((error, opening)) => Err((error, Box::new(transaction), Box::new(opening))),
        }
    }
}

impl<'runtime, A: crate::Addin> OpeningTxn<'runtime, A, GenerationStaged<A>> {
    pub(crate) fn install_lifecycle(
        self,
        access: &AddinLifecycleAccess<'_, A>,
    ) -> Result<OpeningTxn<'runtime, A, LifecycleInstalled<A>>, LifecycleInstallFailure<'runtime, A>>
    {
        let mut transaction = self;
        let GenerationStaged {
            core,
            host,
            lifecycle,
        } = transaction.take_state();
        match transaction.deps.install_addin_lifecycle(access, lifecycle) {
            Ok(()) => Ok(OpeningTxn::new_state(
                transaction.deps,
                LifecycleInstalled { core, host },
            )),
            Err(error) => {
                transaction.state = Some(GenerationStaged {
                    core,
                    host,
                    lifecycle: error.value,
                });
                Err((error.reason, Box::new(transaction)))
            }
        }
    }
}

impl<'runtime, A: crate::Addin> OpeningTxn<'runtime, A, LifecycleInstalled<A>> {
    pub(crate) fn stage_host_mutations(
        self,
        registrations: Vec<RegistrationId>,
    ) -> OpeningTxn<'runtime, A, HostMutated<A>> {
        let mut transaction = self;
        let LifecycleInstalled { core, mut host } = transaction.take_state();
        host.stage_registrations(registrations);
        OpeningTxn::new_state(transaction.deps, HostMutated { core, host })
    }

    #[cfg(test)]
    pub(crate) fn fail_for_test(self) -> OpenFailureDisposition {
        let mut transaction = self;
        let attempt_id = transaction.attempt_id();
        let disposition =
            crate::runtime::orchestration::LifecycleOrchestrator::new(transaction.deps)
                .mark_open_failed(attempt_id);
        let LifecycleInstalled { core, host: _ } = transaction.take_state();
        let (_, module_opening, _service_inputs) = core.into_parts();
        transaction
            .deps
            .lifecycle_control()
            .complete_open_abort(module_opening.rollback(|| {}));
        disposition
    }

    #[cfg(any(test, feature = "bench-internals"))]
    pub(crate) fn finish_in_place(&mut self, registrations: Vec<RegistrationId>) -> XllResult<()> {
        let state = self.take_state();
        OpeningTxn::new_state(self.deps, state)
            .stage_host_mutations(registrations)
            .commit()
    }
}

impl<'runtime, A: crate::Addin> OpeningTxn<'runtime, A, HostMutated<A>> {
    pub(crate) fn commit(self) -> XllResult<()> {
        let mut transaction = self;
        validate_commit_preconditions(&transaction)?;
        let HostMutated { core, host } = transaction.take_state();
        let (_callbacks, mut journal) = host.into_parts();
        commit_inner(transaction.deps, core, &mut journal)
    }
}

impl<'runtime, A: crate::Addin, S> OpeningTxn<'runtime, A, S>
where
    S: OpeningState<A> + HasHost,
{
    pub(crate) fn callbacks_mut(&mut self) -> &mut HostCallbackSession {
        &mut self
            .state
            .as_mut()
            .expect("opening transaction state already consumed")
            .host_mut()
            .callbacks
    }

    pub(crate) fn retain_journal(&mut self, journal: HostMutationJournal) {
        self.state
            .as_mut()
            .expect("opening transaction state already consumed")
            .host_mut()
            .journal
            .merge(journal);
    }
}

fn validate_commit_preconditions<A: crate::Addin, S: OpeningState<A>>(
    transaction: &OpeningTxn<'_, A, S>,
) -> XllResult<()> {
    let control = transaction.deps.lifecycle_access();
    transaction
        .deps
        .lifecycle_control()
        .validate_open_attempt(&control, transaction.attempt_id())
}

fn commit_inner<A: crate::Addin>(
    deps: OpenDeps<'_, A>,
    core: InitializedOpenCore<A>,
    journal: &mut HostMutationJournal,
) -> XllResult<()> {
    let (attempt_id, module_opening, service_inputs) = core.into_parts();
    let mut control = deps.lifecycle_access();
    let registration_ids = journal
        .pending_registrations
        .iter()
        .map(|entry| entry.registration)
        .collect::<Vec<_>>();
    deps.clear_metadata_debt_for_registrations(&registration_ids);
    deps.merge_host(std::mem::take(journal));

    if control.phase() == crate::lifecycle::LifecyclePhase::Opening {
        let mut module_opening = Some(module_opening);
        let ingress = crate::module_runtime::ingress();
        let generation = attempt_id.into_runtime_generation();
        let result = ingress
            .complete_open(|| {
                publish_generation(
                    deps,
                    attempt_id,
                    &mut control,
                    &mut module_opening,
                    service_inputs,
                )?;
                deps.lifecycle_control()
                    .finish_open_state(&mut control, generation)?;
                if control.phase() != crate::lifecycle::LifecyclePhase::Open
                    || control.last_committed_generation() != Some(generation)
                    || control.open_attempt().is_some()
                {
                    crate::boundary::fail_stop_invariant(
                        "xlAutoOpen commit postcondition",
                        &XllError::Internal {
                            diagnostic_id: crate::diagnostics::id::DiagnosticId::OPEN_STATE,
                        },
                    );
                }
                deps.observer()
                    .commit_open(&deps, &control, attempt_id, generation);
                Ok::<(), XllError>(())
            })
            .unwrap_or_else(|_| opening_publication_lost());

        match result {
            Ok(()) => {
                deps.lifecycle().notify_all();
                Ok(())
            }
            Err(error) => {
                recover_uncommitted_module(deps, &mut module_opening, &mut control);
                drop(control);
                let lifecycle = deps.lifecycle_control();
                let mut control = lifecycle.access();
                lifecycle.quarantine_state(&mut control);
                drop(control);
                lifecycle.notify_all();
                Err(error)
            }
        }
    } else {
        let authority = deps.lifecycle_control();
        let closing = module_opening.rollback(|| {});
        authority.complete_open_abort_locked(&mut control, closing);
        // Clearing the open attempt makes final close claimable. Record that
        // transition before a close waiter can acquire the lifecycle lock.
        deps.observer().reject_open_locked(&control, attempt_id);
        deps.lifecycle().notify_all();
        drop(control);
        Err(XllError::Closing)
    }
}

fn publish_generation<A: crate::Addin>(
    deps: OpenDeps<'_, A>,
    attempt_id: OpenAttemptId,
    control: &mut LifecycleAccess<'_, A>,
    module_opening: &mut Option<crate::module_runtime::ModuleOpening>,
    service_inputs: crate::runtime_components::GenerationServiceInputs<
        crate::addin::AddinSubscription<A>,
        crate::addin::AddinSources<A>,
    >,
) -> XllResult<()> {
    let generation = attempt_id.into_runtime_generation();
    let config = control.opening_config().ok_or(XllError::Internal {
        diagnostic_id: crate::diagnostics::id::DiagnosticId::OPEN_STATE,
    })?;
    #[cfg(feature = "rtd")]
    let subscription_host =
        crate::excel_rtd::RtdSubscriptionHost::production(crate::module_runtime::ingress());
    let services = GenerationServices::arm_generation(
        generation,
        config,
        #[cfg(feature = "rtd")]
        subscription_host,
        service_inputs,
    )?
    .commit();
    let module_epoch = module_opening
        .take()
        .expect("open transaction owns its module opening authority")
        .commit();
    match deps.lifecycle_control().publish_generation_state(
        control,
        generation,
        services,
        module_epoch,
    ) {
        Ok(()) => Ok(()),
        Err(failure) => {
            let (error, opening, services, module_epoch) = *failure;
            services.disarm_or_abort();
            if let Some(opening) = opening {
                let crate::generation::OpeningGeneration {
                    shared_state,
                    layers,
                    init_config: _,
                } = opening;
                deps.quarantine().retain_generation(
                    Some(generation),
                    crate::generation::ExecutionGeneration {
                        id: generation,
                        shared_state,
                        layers,
                    },
                    crate::runtime_components::QuarantineReason::OpenStateInvariant,
                );
            }
            let closing = module_epoch.begin_close(|| {});
            deps.lifecycle_control()
                .complete_open_abort_locked(control, closing);
            Err(error)
        }
    }
}

fn recover_uncommitted_module<A: crate::Addin>(
    deps: OpenDeps<'_, A>,
    module_opening: &mut Option<crate::module_runtime::ModuleOpening>,
    control: &mut LifecycleAccess<'_, A>,
) {
    if let Some(module_opening) = module_opening.take() {
        deps.lifecycle_control()
            .complete_open_abort_locked(control, module_opening.rollback(|| {}));
    }
}

impl<A: crate::Addin, S: OpeningState<A>> Drop for OpeningTxn<'_, A, S> {
    fn drop(&mut self) {
        if let Some(state) = self.state.take() {
            state.abandon(&self.deps);
        }
    }
}

#[cold]
fn opening_publication_lost() -> ! {
    crate::boundary::fail_stop_invariant(
        "xlAutoOpen opening publication",
        &XllError::Internal {
            diagnostic_id: crate::diagnostics::id::DiagnosticId::OPEN_STATE,
        },
    )
}

#[cfg(test)]
mod owner_tests {
    use super::*;
    use crate::lifecycle::LifecyclePhase;
    use crate::module_runtime::{ModuleCleanupAuthority, ModuleCleanupClaim, ModuleCleanupReady};
    use crate::runtime::Runtime;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    struct RetainedLifecycle(Arc<AtomicUsize>);
    impl Drop for RetainedLifecycle {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    struct Probe;
    impl crate::Addin for Probe {
        #[cfg(feature = "rtd")]
        type RtdSubscription = ();
        #[cfg(feature = "rtd")]
        type RtdSources = crate::DynamicRtdSources<()>;
        type SharedState = ();
        type LifecycleState = RetainedLifecycle;
        type Layers = ();
        type Error = XllError;
        #[cfg(feature = "async")]
        type AsyncExecutor = crate::NoAsyncExecutor;
        fn open(_: &crate::OpenContext<Self>) -> crate::OpenResult<Self> {
            unreachable!("test initializes state directly")
        }
    }

    #[test]
    fn acquisition_failure_retains_responsibility_before_and_after_module_begin() {
        for point in [1, 2] {
            let runtime = Runtime::<()>::new();
            let other = Runtime::<()>::new();
            FAIL_OPEN_ACQUIRE.with(|fault| fault.set(point));
            assert!(matches!(runtime.begin_open(), Err(XllError::Closing)));
            FAIL_OPEN_ACQUIRE.with(|fault| assert_eq!(fault.get(), 0));
            assert_eq!(runtime.phase(), LifecyclePhase::Quarantined);
            assert_eq!(other.phase(), LifecyclePhase::Closed);
            assert!(runtime.return_protocol.returns_closed_and_quiescent());
            let authority = runtime.lifecycle_control().take_module_cleanup_authority();
            if point == 1 {
                assert!(matches!(authority, ModuleCleanupClaim::Missing));
                assert!(runtime.lifecycle.access().module_epoch_id().is_none());
            } else {
                let ModuleCleanupClaim::Ready(ModuleCleanupReady::Closing(closing)) = authority
                else {
                    panic!("module authority lost during acquisition");
                };
                assert!(closing.id().is_current());
                runtime
                    .lifecycle_control()
                    .retain_failed_module_cleanup(ModuleCleanupAuthority::Closing(closing));
            }
        }
    }

    fn initialized<'a>(
        runtime: &'a Runtime<Probe>,
        drops: &Arc<AtomicUsize>,
    ) -> OpeningTxn<'a, Probe, Initialized<Probe>> {
        let transaction = runtime
            .begin_open()
            .unwrap()
            .attach_host(HostCallbackSession::new());
        let generation = transaction.attempt_id().into_runtime_generation();
        transaction.begin_initialization().initialized(
            RetainedLifecycle(Arc::clone(drops)),
            crate::runtime_components::GenerationServiceInputs::empty_for_generation(generation),
        )
    }

    #[test]
    fn abandoned_rollback_retains_issuer_module_and_untrusted_lifecycle() {
        let runtime = Runtime::<Probe>::new();
        let other = Runtime::<Probe>::new();
        let drops = Arc::new(AtomicUsize::new(0));
        let rollback = initialized(&runtime, &drops).into_rollback();
        let epoch = crate::module_runtime::ingress().epoch();
        drop(rollback);
        assert_eq!(runtime.phase(), LifecyclePhase::Quarantined);
        assert_eq!(other.phase(), LifecyclePhase::Closed);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        let ModuleCleanupClaim::Ready(ModuleCleanupReady::Closing(closing)) =
            runtime.lifecycle_control().take_module_cleanup_authority()
        else {
            panic!("abandoned rollback lost authority");
        };
        assert!(closing.id().is_current());
        assert_eq!(crate::module_runtime::ingress().epoch(), epoch);
        runtime
            .lifecycle_control()
            .retain_failed_module_cleanup(ModuleCleanupAuthority::Closing(closing));
    }

    #[test]
    fn rollback_unwind_after_module_transfer_keeps_original_authority() {
        let runtime = Runtime::<Probe>::new();
        let other = Runtime::<Probe>::new();
        let drops = Arc::new(AtomicUsize::new(0));
        let rollback = initialized(&runtime, &drops).into_rollback();
        let epoch = crate::module_runtime::ingress().epoch();
        let lifecycle = runtime.bind_addin_lifecycle().unwrap();
        PANIC_ROLLBACK_TRANSFER.with(|fault| fault.set(true));
        assert!(
            crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(
                || rollback.run(&lifecycle)
            ))
            .is_err()
        );
        assert_eq!(runtime.phase(), LifecyclePhase::Quarantined);
        assert_eq!(other.phase(), LifecyclePhase::Closed);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        let ModuleCleanupClaim::Ready(ModuleCleanupReady::Closing(closing)) =
            runtime.lifecycle_control().take_module_cleanup_authority()
        else {
            panic!("unwind lost transferred authority");
        };
        assert!(closing.id().is_current());
        assert_eq!(crate::module_runtime::ingress().epoch(), epoch);
        runtime
            .lifecycle_control()
            .retain_failed_module_cleanup(ModuleCleanupAuthority::Closing(closing));
    }

    #[test]
    fn rollback_foreign_lifecycle_access_quarantines_only_its_issuer() {
        let runtime = Runtime::<Probe>::new();
        let other = Runtime::<Probe>::new();
        let drops = Arc::new(AtomicUsize::new(0));
        let rollback = initialized(&runtime, &drops).into_rollback();
        let other_lifecycle = other.bind_addin_lifecycle().unwrap();
        rollback.run(&other_lifecycle);
        assert_eq!(runtime.phase(), LifecyclePhase::Quarantined);
        assert_eq!(other.phase(), LifecyclePhase::Closed);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        assert!(other.lifecycle.access().module_epoch_id().is_none());
    }
}
