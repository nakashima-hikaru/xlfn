use crate::generation::{OpenAttemptId, OpeningGeneration, RemovalEpoch};
use crate::host_callback::HostCallbackSession;
use crate::ingress::OpeningPublication;
#[cfg(test)]
use crate::lifecycle::OpenFailureDisposition;
use crate::lifecycle::{
    CanonicalGenerationPublished, LifecycleAccess, LifecycleControl, LifecyclePhase, OpenStart,
    UnpublishedGeneration,
};
use crate::module_runtime::{ModuleEpochLease, ModuleOpening};
use crate::registration::HostMutationJournal;
#[cfg(any(test, feature = "bench-internals"))]
use crate::registration::RegistrationId;
use crate::runtime::AddinLifecycleAccess;
use crate::runtime::capabilities::{InstalledLifecycle, OpenDeps};
use crate::runtime_components::GenerationServices;
use crate::{XllError, XllResult};
use xlfn_kernel::rollback::{Rollback, RollbackGuard};
use xlfn_kernel::thread_affine::ThreadAffineError;

/// A state payload in the open transaction protocol.
pub(crate) trait OpeningState<A: crate::Addin>: Sized {
    type Binding<'runtime>;
    fn attempt_id(&self) -> OpenAttemptId;
    fn abandon(self, deps: &OpenDeps<'_, A>, binding: Self::Binding<'_>);
}

type RawServiceInputs<A> = crate::runtime_components::GenerationServiceInputs<
    crate::addin_impl::AddinSubscription<A>,
    crate::addin_impl::AddinSources<A>,
>;
// Only recovery erases which payload the opening owner currently holds.
pub(crate) enum RollbackInputs<A: crate::Addin> {
    Empty,
    Raw(RawServiceInputs<A>),
    Prepared(PreparedServices<A>),
}

impl<A: crate::Addin> From<()> for RollbackInputs<A> {
    fn from((): ()) -> Self {
        Self::Empty
    }
}

impl<A: crate::Addin> From<RawServiceInputs<A>> for RollbackInputs<A> {
    fn from(inputs: RawServiceInputs<A>) -> Self {
        Self::Raw(inputs)
    }
}

impl<A: crate::Addin> From<PreparedServices<A>> for RollbackInputs<A> {
    fn from(services: PreparedServices<A>) -> Self {
        Self::Prepared(services)
    }
}

impl<A: crate::Addin> RollbackInputs<A> {
    fn dispose(self) -> bool {
        match self {
            Self::Empty => true,
            Self::Raw(inputs) => discard_open_inputs(inputs),
            Self::Prepared(services) => discard_open_inputs(services),
        }
    }
}

type InitializedOpenCore<A> = OpenCore<A, RawServiceInputs<A>>;
type RollbackOpenCore<A> = OpenCore<A, RollbackInputs<A>>;
type PreparedOpenCore<A> = OpenCore<A, PreparedServices<A>>;

/// Service ownership follows initialization and preparation as distinct types.
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

    fn initialized(self, inputs: RawServiceInputs<A>) -> InitializedOpenCore<A> {
        self.map_inputs(|()| inputs)
    }
}

impl<A: crate::Addin, I: Into<RollbackInputs<A>>> OpenCore<A, I> {
    fn into_rollback(self) -> RollbackOpenCore<A> {
        self.map_inputs(Into::into)
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
pub(crate) struct GenerationStaged<A: crate::Addin, I = RawServiceInputs<A>> {
    core: OpenCore<A, I>,
    config: crate::addin_impl::RuntimeConfig,
    host: HostOpeningState,
    lifecycle: A::LifecycleState,
}

/// All private services are armed, but still owned by the opening attempt.
pub(crate) type PreparedGeneration<A> = GenerationStaged<A, PreparedServices<A>>;

/// The lifecycle payload has been transferred to the runtime's thread-affine
/// slot. The transaction still owns the module and host capabilities.
pub(crate) struct LifecycleInstalled<A: crate::Addin> {
    core: PreparedOpenCore<A>,
    host: HostOpeningState,
}

/// All mandatory open steps are complete and the transaction may commit.
pub(crate) struct HostMutated<A: crate::Addin> {
    core: PreparedOpenCore<A>,
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
}

/// Lifecycle ownership while an opening transaction is being rolled back.
/// The enum is a recovery projection, not an independent state machine: the
/// production transaction states above determine which variant can be made.
pub(crate) enum LifecycleOwnership<'runtime, A: crate::Addin> {
    Owned(A::LifecycleState),
    Installed(InstalledLifecycle<'runtime, A>),
    Uninstalled,
}

pub(crate) struct RollbackParts<'runtime, A: crate::Addin> {
    core: RollbackOpenCore<A>,
    host: HostOpeningState,
    lifecycle: LifecycleOwnership<'runtime, A>,
}

/// States that still own the host-side rollback session.
pub(crate) trait RollbackState<A: crate::Addin>: OpeningState<A> {
    fn into_rollback_parts<'runtime>(
        self,
        binding: Self::Binding<'runtime>,
    ) -> RollbackParts<'runtime, A>;
}

fn forget_untrusted_lifecycle_state<A: crate::Addin>(state: A::LifecycleState) {
    #[allow(
        clippy::mem_forget,
        reason = "an abandoned open transaction must not run untrusted lifecycle destruction"
    )]
    std::mem::forget(state);
}

fn discard_open_inputs<I>(inputs: I) -> bool {
    if crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(|| drop(inputs)))
        .is_err()
    {
        crate::boundary::report_boundary_error("xlAutoOpen service inputs", &XllError::Panic);
        false
    } else {
        true
    }
}

fn abandon_open<A: crate::Addin, I>(
    deps: &OpenDeps<'_, A>,
    core: OpenCore<A, I>,
    lifecycle: LifecycleOwnership<'_, A>,
) {
    deps.returns().close_admission();
    let (_, module_opening, service_inputs) = core.into_parts();
    // Source catalog destruction is application code. Keep the module and
    // opening responsibility until it finishes, and contain its panic even
    // when abandonment itself is running during an unwind.
    discard_open_inputs(service_inputs);
    let control_api = deps.lifecycle_control();
    let mut control = control_api.access();
    return_open_module_locked(*deps, &mut control, module_opening.rollback(|| {}));
    control_api.record_open_failure(&mut control);
    control_api.quarantine_state(&mut control);
    drop(control);
    control_api.notify_all();
    if let LifecycleOwnership::Owned(state) = lifecycle {
        forget_untrusted_lifecycle_state::<A>(state);
    }
}

fn return_open_module<A: crate::Addin>(
    deps: &OpenDeps<'_, A>,
    closing: crate::module_runtime::ModuleClosing,
) {
    let lifecycle = deps.lifecycle_control();
    let mut control = lifecycle.access();
    return_open_module_locked(*deps, &mut control, closing);
}

fn return_open_module_locked<A: crate::Addin>(
    deps: OpenDeps<'_, A>,
    control: &mut LifecycleAccess<'_, A>,
    closing: crate::module_runtime::ModuleClosing,
) {
    if let Err(authority) = deps
        .lifecycle_control()
        .complete_open_abort_locked(control, closing)
    {
        // The shared target changed or already owns another authority. Keep
        // both authorities; rejection must not manufacture a successful abort.
        deps.quarantine().retain_module_authority(authority);
        deps.lifecycle_control().quarantine_state(control);
    }
}

impl<A: crate::Addin> OpeningState<A> for Begun<A> {
    type Binding<'runtime> = ();
    fn attempt_id(&self) -> OpenAttemptId {
        self.core.attempt_id()
    }

    fn abandon(self, deps: &OpenDeps<'_, A>, _binding: Self::Binding<'_>) {
        let Self { core } = self;
        abandon_open(deps, core, LifecycleOwnership::Uninstalled);
    }
}

impl<A: crate::Addin> OpeningState<A> for HostAttached<A> {
    type Binding<'runtime> = ();
    fn attempt_id(&self) -> OpenAttemptId {
        self.core.attempt_id()
    }

    fn abandon(self, deps: &OpenDeps<'_, A>, _binding: Self::Binding<'_>) {
        let Self { core, host: _ } = self;
        abandon_open(deps, core, LifecycleOwnership::Uninstalled);
    }
}

impl<A: crate::Addin> OpeningState<A> for Initializing<A> {
    type Binding<'runtime> = ();
    fn attempt_id(&self) -> OpenAttemptId {
        self.core.attempt_id()
    }

    fn abandon(self, deps: &OpenDeps<'_, A>, _binding: Self::Binding<'_>) {
        let Self { core, host: _ } = self;
        let attempt = core.attempt_id();
        let reason = if std::thread::panicking() {
            crate::shutdown_trace::ShutdownFailure::BoundaryPanic
        } else {
            crate::shutdown_trace::ShutdownFailure::AddinShutdownFailed
        };
        abandon_open(deps, core, LifecycleOwnership::Uninstalled);
        deps.observer().quarantine_open(attempt, reason);
    }
}

impl<A: crate::Addin> OpeningState<A> for Initialized<A> {
    type Binding<'runtime> = ();
    fn attempt_id(&self) -> OpenAttemptId {
        self.core.attempt_id()
    }

    fn abandon(self, deps: &OpenDeps<'_, A>, _binding: Self::Binding<'_>) {
        let Self {
            core,
            host: _,
            lifecycle,
        } = self;
        abandon_open(deps, core, LifecycleOwnership::Owned(lifecycle));
    }
}

impl<A: crate::Addin, I> OpeningState<A> for GenerationStaged<A, I> {
    type Binding<'runtime> = ();
    fn attempt_id(&self) -> OpenAttemptId {
        self.core.attempt_id()
    }

    fn abandon(self, deps: &OpenDeps<'_, A>, _binding: Self::Binding<'_>) {
        let Self {
            core,
            host,
            lifecycle,
            ..
        } = self;
        let (_, journal) = host.into_parts();
        deps.merge_host(journal);
        abandon_open(deps, core, LifecycleOwnership::Owned(lifecycle));
    }
}

impl<A: crate::Addin> OpeningState<A> for LifecycleInstalled<A> {
    type Binding<'runtime> = InstalledLifecycle<'runtime, A>;
    fn attempt_id(&self) -> OpenAttemptId {
        self.core.attempt_id()
    }

    fn abandon(self, deps: &OpenDeps<'_, A>, _binding: Self::Binding<'_>) {
        let Self { core, host } = self;
        let (_, journal) = host.into_parts();
        deps.merge_host(journal);
        abandon_open(deps, core, LifecycleOwnership::Installed(_binding));
    }
}

impl<A: crate::Addin> OpeningState<A> for HostMutated<A> {
    type Binding<'runtime> = InstalledLifecycle<'runtime, A>;
    fn attempt_id(&self) -> OpenAttemptId {
        self.core.attempt_id()
    }

    fn abandon(self, deps: &OpenDeps<'_, A>, _binding: Self::Binding<'_>) {
        let Self { core, host } = self;
        let (_, journal) = host.into_parts();
        deps.merge_host(journal);
        abandon_open(deps, core, LifecycleOwnership::Installed(_binding));
    }
}

impl<A: crate::Addin> RollbackState<A> for HostAttached<A> {
    fn into_rollback_parts<'runtime>(
        self,
        _binding: Self::Binding<'runtime>,
    ) -> RollbackParts<'runtime, A> {
        let Self { core, host } = self;
        RollbackParts {
            core: core.into_rollback(),
            host,
            lifecycle: LifecycleOwnership::Uninstalled,
        }
    }
}

impl<A: crate::Addin> RollbackState<A> for Initialized<A> {
    fn into_rollback_parts<'runtime>(
        self,
        _binding: Self::Binding<'runtime>,
    ) -> RollbackParts<'runtime, A> {
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

impl<A: crate::Addin, I: Into<RollbackInputs<A>>> RollbackState<A> for GenerationStaged<A, I> {
    fn into_rollback_parts<'runtime>(
        self,
        _binding: Self::Binding<'runtime>,
    ) -> RollbackParts<'runtime, A> {
        let Self {
            core,
            host,
            lifecycle,
            ..
        } = self;
        RollbackParts {
            core: core.into_rollback(),
            host,
            lifecycle: LifecycleOwnership::Owned(lifecycle),
        }
    }
}

impl<A: crate::Addin> RollbackState<A> for LifecycleInstalled<A> {
    fn into_rollback_parts<'runtime>(
        self,
        _binding: Self::Binding<'runtime>,
    ) -> RollbackParts<'runtime, A> {
        let Self { core, host } = self;
        RollbackParts {
            core: core.into_rollback(),
            host,
            lifecycle: LifecycleOwnership::Installed(_binding),
        }
    }
}

impl<A: crate::Addin> RollbackState<A> for HostMutated<A> {
    fn into_rollback_parts<'runtime>(
        self,
        _binding: Self::Binding<'runtime>,
    ) -> RollbackParts<'runtime, A> {
        let Self { core, host } = self;
        RollbackParts {
            core: core.into_rollback(),
            host,
            lifecycle: LifecycleOwnership::Installed(_binding),
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

impl<A: crate::Addin> HasHost for PreparedGeneration<A> {
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

/// Owns the canonical attempt and its original lock before module acquisition.
struct StartClaimed<'runtime, A: crate::Addin>(RollbackGuard<PendingStart<'runtime, A>>);

struct PendingStart<'runtime, A: crate::Addin> {
    deps: OpenDeps<'runtime, A>,
    start: OpenStart<'runtime, A>,
}

impl<A: crate::Addin> Rollback for PendingStart<'_, A> {
    fn rollback(self) {
        self.deps.returns().close_admission();
        drop(self.start);
    }
}

impl<'runtime, A: crate::Addin> StartClaimed<'runtime, A> {
    fn open_module(self) -> XllResult<ModuleOpened<'runtime, A>> {
        self.0.get().deps.returns().reopen_admission()?;
        // Until begin_open returns there is no authority to recover a partly
        // changed module epoch.
        let transfer = std::mem::DropGuard::new((), |()| opening_publication_lost());
        let module = match crate::module_runtime::begin_open() {
            Ok(module) => module,
            Err(error) => {
                // The callback gate refused before any module mutation.
                std::mem::DropGuard::dismiss(transfer);
                return Err(error);
            }
        };
        let PendingStart { deps, start } = self.0.complete();
        let opened = ModuleOpened(RollbackGuard::new(PendingModule {
            deps,
            start,
            module,
        }));
        std::mem::DropGuard::dismiss(transfer);
        Ok(opened)
    }
}

struct ModuleOpened<'runtime, A: crate::Addin>(RollbackGuard<PendingModule<'runtime, A>>);

struct PendingModule<'runtime, A: crate::Addin> {
    deps: OpenDeps<'runtime, A>,
    start: OpenStart<'runtime, A>,
    module: ModuleOpening,
}

impl<A: crate::Addin> Rollback for PendingModule<'_, A> {
    fn rollback(self) {
        let Self {
            deps,
            mut start,
            module,
        } = self;
        deps.returns().close_admission();
        if let Err(authority) = start.return_module(module.rollback(|| {})) {
            deps.quarantine().retain_module_authority(authority);
        }
        drop(start);
    }
}

impl<'runtime, A: crate::Addin> ModuleOpened<'runtime, A> {
    fn issue(
        self,
        expected_removal_epoch: RemovalEpoch,
    ) -> XllResult<OpeningTxn<'runtime, A, Begun<A>>> {
        let pending = self.0.get();
        let attempt = pending.start.attempt();
        pending.deps.observer().begin_open(
            pending.start.access(),
            pending.deps.returns(),
            expected_removal_epoch.get(),
            attempt,
        );
        let PendingModule {
            deps,
            start,
            module,
        } = self.0.complete();
        let successor = OpeningTxn::new_state(
            deps,
            Begun {
                core: OpenCore::new(attempt, module),
            },
            (),
        );
        // A failed handoff still owns both recoveries. It unlocks the start
        // before dropping the successor, which can reenter the coordinator.
        start.transfer_to_opening(successor).map_err(|failure| {
            drop(failure);
            open_state_error()
        })
    }
}

/// Rollback retains its issuer and responsibility across lifecycle installation,
/// failure publication and the complete cleanup operation.
pub(crate) struct OpenRollbackOwner<'runtime, A: crate::Addin> {
    pending: RollbackGuard<PrepareOpenRollback<'runtime, A>>,
}

/// Keep host and module recovery armed while lifecycle installation consumes
/// the lifecycle value. A refused installation gives the value back.
struct PrepareOpenRollback<'runtime, A: crate::Addin> {
    deps: OpenDeps<'runtime, A>,
    core: RollbackOpenCore<A>,
    host: HostOpeningState,
    lifecycle: Option<LifecycleOwnership<'runtime, A>>,
}

impl<A: crate::Addin> Rollback for PrepareOpenRollback<'_, A> {
    fn rollback(self) {
        let (_, journal) = self.host.into_parts();
        self.deps.merge_host(journal);
        let lifecycle = self.lifecycle.unwrap_or(LifecycleOwnership::Uninstalled);
        abandon_open(&self.deps, self.core, lifecycle);
    }
}

/// After handing off the journal, recovery still owns the opening module.
struct ReturnOpenModule<'runtime, A: crate::Addin> {
    deps: OpenDeps<'runtime, A>,
    core: RollbackOpenCore<A>,
}

impl<A: crate::Addin> Rollback for ReturnOpenModule<'_, A> {
    fn rollback(self) {
        abandon_open(&self.deps, self.core, LifecycleOwnership::Uninstalled);
    }
}

/// Once module return starts, recovery can quarantine the issuer while the
/// module authority travels through its own consuming return protocol.
struct QuarantineOpen<'runtime, A: crate::Addin>(OpenDeps<'runtime, A>);

impl<A: crate::Addin> Rollback for QuarantineOpen<'_, A> {
    fn rollback(self) {
        let lifecycle = self.0.lifecycle_control();
        let mut control = lifecycle.access();
        self.0.returns().close_admission();
        lifecycle.quarantine_state(&mut control);
        lifecycle.notify_all();
    }
}

impl<A: crate::Addin> OpenRollbackOwner<'_, A> {
    pub(crate) fn run(mut self, lifecycle: &AddinLifecycleAccess<'_, A>) {
        use crate::boundary::report_boundary_error;
        let deps = self.pending.get().deps;
        let recovery = deps.recovery();
        // Release callback and lifecycle capabilities before either rollback
        // guard reenters the issuer on unwind.
        let quarantine;
        let mut return_module;
        let installed = match self.pending.get_mut().lifecycle.take() {
            Some(LifecycleOwnership::Installed(binding)) => Some(binding),
            Some(LifecycleOwnership::Owned(state)) => {
                match deps.install_addin_lifecycle(lifecycle, state) {
                    Ok(binding) => Some(binding),
                    Err(error) => {
                        let (state, reason) = error.into_parts();
                        self.pending.get_mut().lifecycle = Some(LifecycleOwnership::Owned(state));
                        report_boundary_error(
                            "xlAutoOpen rollback lifecycle installation",
                            &crate::lifecycle::lifecycle_access_error(reason),
                        );
                        drop(self);
                        recovery.quarantine(None);
                        return;
                    }
                }
            }
            Some(LifecycleOwnership::Uninstalled) | None => None,
        };
        let PrepareOpenRollback { core, host, .. } = self.pending.complete();
        return_module = RollbackGuard::new(ReturnOpenModule { deps, core });
        let (mut callbacks, journal) = host.into_parts();
        deps.merge_host(journal);
        #[cfg(test)]
        check_rollback_fault(1);
        let attempt = return_module.get().core.attempt_id();
        let request = match deps.rollback_request(attempt, installed) {
            Ok(request) => request,
            Err(failure) => {
                report_boundary_error("xlAutoOpen rollback issuer", failure.error());
                // Publish quarantine before the retained claim can be released.
                recovery.quarantine(None);
                drop(failure);
                return;
            }
        };
        // A final-removal waiter can claim the module as soon as it is
        // returned. No application-owned service input may outlive that
        // handoff, including a source destructor that reenters lifecycle code.
        let service_inputs = std::mem::replace(
            &mut return_module.get_mut().core.service_inputs,
            RollbackInputs::Empty,
        );
        if !service_inputs.dispose() {
            drop(request);
            drop(return_module);
            recovery.quarantine(None);
            return;
        }
        let disposition =
            super::orchestration::LifecycleOrchestrator::new(deps).mark_open_failed(attempt);
        let (_, module, _service_inputs) = return_module.complete().core.into_parts();
        quarantine = RollbackGuard::new(QuarantineOpen(deps));
        return_open_module(&deps, module.rollback(|| {}));
        #[cfg(test)]
        check_rollback_fault(2);
        if disposition.requires_rollback() {
            match crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(|| {
                request.run(lifecycle, &mut callbacks)
            })) {
                Ok(outcome) if outcome.is_finalized() || outcome.cleanup_owned_elsewhere() => {}
                Ok(outcome) => {
                    let error = crate::error::construct::internal(
                        crate::diagnostics::catalog::OPEN_ROLLBACK_FAILURE,
                    );
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
        quarantine.complete();
    }
}

#[cfg(test)]
std::thread_local! {
    static FAIL_PREPARE_SERVICES: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FAIL_OPEN_ACQUIRE: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
    static FAIL_OPEN_COMMIT: std::cell::Cell<(u8, bool)> = const { std::cell::Cell::new((0, false)) };
    static PANIC_ROLLBACK_TRANSFER: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}
#[cfg(test)]
fn check_prepare_services_fault() -> XllResult<()> {
    if FAIL_PREPARE_SERVICES.with(|fault| fault.replace(false)) {
        Err(open_state_error())
    } else {
        Ok(())
    }
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
fn check_open_commit_fault(point: u8) -> XllResult<()> {
    FAIL_OPEN_COMMIT.with(|fault| {
        let (stage, unwinding) = fault.get();
        if stage == point {
            fault.set((0, false));
            assert!(!unwinding, "injected open commit panic at stage {point}");
            Err(XllError::Closing)
        } else {
            Ok(())
        }
    })
}
#[cfg(test)]
fn check_rollback_fault(point: u8) {
    PANIC_ROLLBACK_TRANSFER.with(|fault| {
        if fault.get() == point {
            fault.set(0);
            panic!("injected rollback transfer panic");
        }
    });
}

pub(crate) struct OpeningTxn<'runtime, A: crate::Addin, S: OpeningState<A>> {
    pending: RollbackGuard<PendingOpening<'runtime, A, S>>,
}

struct PendingOpening<'runtime, A: crate::Addin, S: OpeningState<A>> {
    deps: OpenDeps<'runtime, A>,
    state: S,
    binding: S::Binding<'runtime>,
}

impl<A: crate::Addin, S: OpeningState<A>> Rollback for PendingOpening<'_, A, S> {
    fn rollback(self) {
        self.state.abandon(&self.deps, self.binding);
    }
}

type OpeningStageFailure<'runtime, A> = (
    XllError,
    Box<OpeningTxn<'runtime, A, Initialized<A>>>,
    Box<OpeningGeneration<A>>,
);

type LifecycleInstallFailure<'runtime, A> = (
    ThreadAffineError,
    Box<OpeningTxn<'runtime, A, PreparedGeneration<A>>>,
);

impl<'runtime, A: crate::Addin, S: OpeningState<A>> OpeningTxn<'runtime, A, S> {
    fn new_state(deps: OpenDeps<'runtime, A>, state: S, binding: S::Binding<'runtime>) -> Self {
        Self {
            pending: RollbackGuard::new(PendingOpening {
                deps,
                state,
                binding,
            }),
        }
    }

    fn into_parts(self) -> (OpenDeps<'runtime, A>, S, S::Binding<'runtime>) {
        let PendingOpening {
            deps,
            state,
            binding,
        } = self.pending.complete();
        (deps, state, binding)
    }

    pub(crate) fn attempt_id(&self) -> OpenAttemptId {
        self.pending.get().state.attempt_id()
    }

    pub(crate) fn deps(&self) -> OpenDeps<'runtime, A> {
        self.pending.get().deps
    }

    pub(crate) fn into_rollback(self) -> OpenRollbackOwner<'runtime, A>
    where
        S: RollbackState<A>,
    {
        let (deps, state, binding) = self.into_parts();
        let RollbackParts {
            core,
            host,
            lifecycle,
        } = state.into_rollback_parts(binding);
        OpenRollbackOwner {
            pending: RollbackGuard::new(PrepareOpenRollback {
                deps,
                core,
                host,
                lifecycle: Some(lifecycle),
            }),
        }
    }
}

impl<'runtime, A: crate::Addin> OpeningTxn<'runtime, A, Begun<A>> {
    /// Read-only identity check used exclusively by the locked handoff boundary.
    /// This does not manufacture or release either recovery responsibility.
    pub(crate) fn matches_start(
        &self,
        coordinator: &crate::lifecycle::LifecycleCoordinator<A>,
        attempt: OpenAttemptId,
    ) -> bool {
        std::ptr::eq(self.deps().lifecycle(), coordinator) && self.attempt_id() == attempt
    }

    pub(crate) fn acquire(
        deps: OpenDeps<'runtime, A>,
        expected_removal_epoch: RemovalEpoch,
    ) -> XllResult<Self> {
        #[cfg(test)]
        let test_module_lease = crate::ingress::acquire_test_module_lease();
        let lifecycle = crate::lifecycle::LifecycleControl::new(deps.lifecycle());
        let start = lifecycle.begin_open(expected_removal_epoch)?;
        // From the first successful canonical mutation, this guard owns both
        // the original lock and the responsibility for an incomplete start.
        let claimed = StartClaimed(RollbackGuard::new(PendingStart { deps, start }));
        #[cfg(test)]
        check_open_acquire_fault(1)?;
        let opened = claimed.open_module()?;
        #[cfg(test)]
        {
            *deps.lifecycle().test_module_lease.lock() = Some(test_module_lease);
        }
        #[cfg(test)]
        check_open_acquire_fault(2)?;
        let transaction = opened.issue(expected_removal_epoch)?;
        #[cfg(test)]
        check_open_acquire_fault(3)?;
        Ok(transaction)
    }

    pub(crate) fn attach_host(
        self,
        callbacks: HostCallbackSession,
    ) -> OpeningTxn<'runtime, A, HostAttached<A>> {
        let (deps, Begun { core }, binding) = self.into_parts();
        OpeningTxn::new_state(
            deps,
            HostAttached {
                core,
                host: HostOpeningState::new(callbacks),
            },
            binding,
        )
    }

    #[cfg(test)]
    pub(crate) fn fail_for_test(self) -> OpenFailureDisposition {
        let attempt_id = self.attempt_id();
        let disposition = crate::runtime::orchestration::LifecycleOrchestrator::new(self.deps())
            .mark_open_failed(attempt_id);
        let (deps, Begun { core }, _binding) = self.into_parts();
        let (_, module_opening, _service_inputs) = core.into_parts();
        return_open_module(&deps, module_opening.rollback(|| {}));
        disposition
    }
}

impl<'runtime, A: crate::Addin> OpeningTxn<'runtime, A, HostAttached<A>> {
    pub(crate) fn begin_initialization(self) -> OpeningTxn<'runtime, A, Initializing<A>> {
        let (deps, HostAttached { core, host }, binding) = self.into_parts();
        OpeningTxn::new_state(deps, Initializing { core, host }, binding)
    }
}

impl<'runtime, A: crate::Addin> OpeningTxn<'runtime, A, Initializing<A>> {
    pub(crate) fn initialized(
        self,
        lifecycle: A::LifecycleState,
        service_inputs: crate::runtime_components::GenerationServiceInputs<
            crate::addin_impl::AddinSubscription<A>,
            crate::addin_impl::AddinSources<A>,
        >,
    ) -> OpeningTxn<'runtime, A, Initialized<A>> {
        let (deps, Initializing { core, host }, binding) = self.into_parts();
        let core = core.initialized(service_inputs);
        OpeningTxn::new_state(
            deps,
            Initialized {
                core,
                host,
                lifecycle,
            },
            binding,
        )
    }
}

impl<'runtime, A: crate::Addin> OpeningTxn<'runtime, A, Initialized<A>> {
    pub(crate) fn stage_opening_generation(
        self,
        opening: OpeningGeneration<A>,
    ) -> Result<OpeningTxn<'runtime, A, GenerationStaged<A>>, OpeningStageFailure<'runtime, A>>
    {
        let config = opening.init_config;
        let result = self
            .deps()
            .lifecycle_control()
            .stage_opening_generation(self.attempt_id(), opening);
        match result {
            Ok(()) => {
                let (
                    deps,
                    Initialized {
                        core,
                        host,
                        lifecycle,
                    },
                    binding,
                ) = self.into_parts();
                Ok(OpeningTxn::new_state(
                    deps,
                    GenerationStaged {
                        core,
                        config,
                        host,
                        lifecycle,
                    },
                    binding,
                ))
            }
            Err((error, opening)) => Err((error, Box::new(self), Box::new(opening))),
        }
    }
}

impl<'runtime, A: crate::Addin> OpeningTxn<'runtime, A, GenerationStaged<A>> {
    pub(crate) fn prepare_services(
        self,
    ) -> Result<OpeningTxn<'runtime, A, PreparedGeneration<A>>, super::open::OpenFailure<'runtime, A>>
    {
        let (
            deps,
            GenerationStaged {
                core,
                config,
                host,
                lifecycle,
            },
            (),
        ) = self.into_parts();
        let (attempt, module, inputs) = core.into_parts();
        // The arm step may consume or discard raw inputs, including running
        // application destructors. Keep recovery armed before entering it.
        let recovery = OpeningTxn::new_state(
            deps,
            GenerationStaged {
                core: OpenCore::<A>::new(attempt, module),
                config,
                host,
                lifecycle,
            },
            (),
        );
        let services = match arm_services::<A>(attempt, config, inputs) {
            Ok(services) => services,
            Err(error) => return Err(super::open::OpenFailure::rollback(recovery, error)),
        };
        let (
            deps,
            GenerationStaged {
                core,
                config,
                host,
                lifecycle,
            },
            (),
        ) = recovery.into_parts();
        Ok(OpeningTxn::new_state(
            deps,
            GenerationStaged {
                core: core.map_inputs(|()| services),
                config,
                host,
                lifecycle,
            },
            (),
        ))
    }
}

fn arm_services<A: crate::Addin>(
    attempt: OpenAttemptId,
    config: crate::addin_impl::RuntimeConfig,
    inputs: RawServiceInputs<A>,
) -> XllResult<PreparedServices<A>> {
    #[cfg(test)]
    check_prepare_services_fault()?;
    #[cfg(feature = "handles")]
    let inputs = {
        let mut inputs = inputs;
        inputs.prepare_handles(config.handle_config())?;
        inputs
    };
    Ok(GenerationServices::arm_generation(
        attempt.into_runtime_generation(),
        config,
        #[cfg(feature = "rtd")]
        crate::excel_rtd::RtdSubscriptionHost::production(crate::module_runtime::ingress()),
        inputs,
    )?
    .commit())
}

impl<'runtime, A: crate::Addin> OpeningTxn<'runtime, A, PreparedGeneration<A>> {
    pub(crate) fn runtime_config(&self) -> crate::addin_impl::RuntimeConfig {
        self.pending.get().state.config
    }

    pub(crate) fn install_lifecycle(
        self,
        access: &AddinLifecycleAccess<'_, A>,
    ) -> Result<OpeningTxn<'runtime, A, LifecycleInstalled<A>>, LifecycleInstallFailure<'runtime, A>>
    {
        let (
            deps,
            GenerationStaged {
                core,
                config,
                host,
                lifecycle,
            },
            _binding,
        ) = self.into_parts();
        match deps.install_addin_lifecycle(access, lifecycle) {
            Ok(binding) => Ok(OpeningTxn::new_state(
                deps,
                LifecycleInstalled { core, host },
                binding,
            )),
            Err(error) => {
                let transaction = OpeningTxn::new_state(
                    deps,
                    GenerationStaged {
                        core,
                        config,
                        host,
                        lifecycle: error.value,
                    },
                    (),
                );
                Err((error.reason, Box::new(transaction)))
            }
        }
    }
}

impl<'runtime, A: crate::Addin> OpeningTxn<'runtime, A, LifecycleInstalled<A>> {
    pub(crate) fn stage_host_mutations(
        self,
        registrations: crate::registration::SuccessfulRegistrations,
    ) -> OpeningTxn<'runtime, A, HostMutated<A>> {
        self.stage_host_journal(registrations.into_journal())
    }

    fn stage_host_journal(
        self,
        journal: HostMutationJournal,
    ) -> OpeningTxn<'runtime, A, HostMutated<A>> {
        let (deps, LifecycleInstalled { core, mut host }, binding) = self.into_parts();
        host.journal.merge(journal);
        OpeningTxn::new_state(deps, HostMutated { core, host }, binding)
    }

    #[cfg(test)]
    pub(crate) fn fail_for_test(self) -> OpenFailureDisposition {
        let attempt_id = self.attempt_id();
        let disposition = crate::runtime::orchestration::LifecycleOrchestrator::new(self.deps())
            .mark_open_failed(attempt_id);
        let (deps, LifecycleInstalled { core, host: _ }, _binding) = self.into_parts();
        let (_, module_opening, _service_inputs) = core.into_parts();
        return_open_module(&deps, module_opening.rollback(|| {}));
        disposition
    }

    #[cfg(any(test, feature = "bench-internals"))]
    pub(super) fn finish(self, registrations: Vec<RegistrationId>) -> XllResult<()> {
        self.stage_host_journal(HostMutationJournal::from_registrations(registrations))
            .commit()
    }
}

impl<'runtime, A: crate::Addin> OpeningTxn<'runtime, A, HostMutated<A>> {
    pub(crate) fn commit(self) -> XllResult<()> {
        validate_commit_preconditions(&self)?;
        let (deps, HostMutated { core, host }, binding) = self.into_parts();
        commit_inner(deps, core, host, binding)
    }
}

impl<'runtime, A: crate::Addin, S> OpeningTxn<'runtime, A, S>
where
    S: OpeningState<A> + HasHost,
{
    pub(crate) fn callbacks_mut(&mut self) -> &mut HostCallbackSession {
        &mut self.pending.get_mut().state.host_mut().callbacks
    }

    pub(crate) fn retain_journal(&mut self, journal: HostMutationJournal) {
        self.pending
            .get_mut()
            .state
            .host_mut()
            .journal
            .merge(journal);
    }
}

fn validate_commit_preconditions<A: crate::Addin, S: OpeningState<A>>(
    transaction: &OpeningTxn<'_, A, S>,
) -> XllResult<()> {
    let deps = transaction.deps();
    let control = deps.lifecycle_access();
    deps.lifecycle_control()
        .validate_open_attempt(&control, transaction.attempt_id())
}

type PreparedServices<A> = Box<
    GenerationServices<crate::addin_impl::AddinSubscription<A>, crate::addin_impl::AddinSources<A>>,
>;

struct OpenCommitContext<'runtime, A: crate::Addin> {
    deps: OpenDeps<'runtime, A>,
    attempt: OpenAttemptId,
    binding: InstalledLifecycle<'runtime, A>,
}

impl<A: crate::Addin> OpenCommitContext<'_, A> {
    fn reject(self, close: impl FnOnce() -> crate::module_runtime::ModuleClosing) {
        let mut control = self.deps.lifecycle_access();
        return_open_module_locked(self.deps, &mut control, close());
        // Publish rejection before the close waiter can claim the attempt.
        self.deps
            .observer()
            .reject_open_locked(&control, self.attempt);
        self.deps.lifecycle().notify_all();
    }

    fn abandon(self, close: impl FnOnce() -> crate::module_runtime::ModuleClosing) {
        self.deps.returns().close_admission();
        let lifecycle = self.deps.lifecycle_control();
        let mut control = lifecycle.access();
        return_open_module_locked(self.deps, &mut control, close());
        lifecycle.record_open_failure(&mut control);
        lifecycle.quarantine_state(&mut control);
        lifecycle.notify_all();
    }
}

struct UncommittedModule<'runtime, A: crate::Addin> {
    context: OpenCommitContext<'runtime, A>,
    module: ModuleOpening,
}

impl<A: crate::Addin> Rollback for UncommittedModule<'_, A> {
    fn rollback(self) {
        let Self { context, module } = self;
        context.abandon(|| module.rollback(|| {}));
    }
}

struct CommitHandoff<'runtime, A: crate::Addin>(RollbackGuard<UncommittedModule<'runtime, A>>);

impl<'runtime, A: crate::Addin> CommitHandoff<'runtime, A> {
    fn prepare(
        self,
        host: HostOpeningState,
        services: PreparedServices<A>,
    ) -> XllResult<ServicesPrepared<'runtime, A>> {
        let deps = self.0.get().context.deps;
        let (_callbacks, journal) = host.into_parts();
        let control = deps.lifecycle_access();
        deps.merge_host(journal);

        if control.phase() != LifecyclePhase::Opening {
            drop(control);
            // Keep this opening attempt responsible until destruction has
            // completed. The close waiter cannot advance to module teardown.
            if !discard_open_inputs(services) {
                return Err(XllError::Panic);
            }
            let UncommittedModule { context, module } = self.0.complete();
            context.reject(|| module.rollback(|| {}));
            return Err(XllError::Closing);
        }
        drop(control);
        #[cfg(test)]
        check_open_commit_fault(1)?;
        Ok(ServicesPrepared {
            services,
            module: self.0,
        })
    }
}

struct ServicesPrepared<'runtime, A: crate::Addin> {
    // Dispose of the private slots while close still waits on this attempt.
    services: PreparedServices<A>,
    module: RollbackGuard<UncommittedModule<'runtime, A>>,
}

impl<'runtime, A: crate::Addin> ServicesPrepared<'runtime, A> {
    fn ready(self) -> PublicationReady<'runtime, A> {
        let Self { services, module } = self;
        let UncommittedModule { context, module } = module.complete();
        // ModuleOpening::commit only repacks the epoch owner. No external
        // effect or callback separates disarming this guard from arming the next.
        let bundle = UnpublishedGeneration::prepare(services, module.commit());
        PublicationReady(RollbackGuard::new(ReadyForPublication { context, bundle }))
    }
}

struct ReadyForPublication<'runtime, A: crate::Addin> {
    context: OpenCommitContext<'runtime, A>,
    bundle: Box<UnpublishedGeneration<A>>,
}

struct UnpublishedEpoch<'runtime, A: crate::Addin> {
    context: OpenCommitContext<'runtime, A>,
    module_epoch: ModuleEpochLease,
}

impl<A: crate::Addin> Rollback for UnpublishedEpoch<'_, A> {
    fn rollback(self) {
        let Self {
            context,
            module_epoch,
        } = self;
        context.abandon(|| module_epoch.begin_close(|| {}));
    }
}

impl<'runtime, A: crate::Addin> ReadyForPublication<'runtime, A> {
    fn dispose_services(self) -> (RollbackGuard<UnpublishedEpoch<'runtime, A>>, bool) {
        let Self { context, bundle } = self;
        let UnpublishedGeneration {
            services,
            module_epoch,
            ..
        } = *bundle;
        let epoch = RollbackGuard::new(UnpublishedEpoch {
            context,
            module_epoch,
        });
        // Keep the epoch recoverable as service destructors run outside the
        // locks. No close waiter may claim or unload this opening yet.
        let disposed = discard_open_inputs(services);
        (epoch, disposed)
    }
}

impl<A: crate::Addin> Rollback for ReadyForPublication<'_, A> {
    fn rollback(self) {
        drop(self.dispose_services());
    }
}

struct PublicationReady<'runtime, A: crate::Addin>(RollbackGuard<ReadyForPublication<'runtime, A>>);

impl<'runtime, A: crate::Addin> PublicationReady<'runtime, A> {
    fn publish(self) -> XllResult<GenerationPublished<'runtime, A>> {
        let deps = self.0.get().context.deps;
        let attempt = self.0.get().context.attempt;
        let lifecycle = LifecycleControl::new(deps.lifecycle());
        let (mut control, ingress) = loop {
            {
                let control = deps.lifecycle().access();
                if control.phase() != LifecyclePhase::Opening {
                    drop(control);
                    return self.reject_interrupted();
                }
                lifecycle.validate_open_attempt(&control, attempt)?;
            }

            // Drain rejected entries without holding lifecycle state. Once
            // admission is held, never block in the reverse lock order.
            let Ok(ingress) = crate::module_runtime::ingress().opening_publication() else {
                return self.reject_interrupted();
            };
            if let Some(control) = deps.lifecycle().try_access() {
                break (control, ingress);
            }
            drop(ingress);
            std::thread::yield_now();
        };

        if !self.0.get().bundle.module_epoch.id().is_current() {
            // Only this opening owns the affine module epoch. A new epoch
            // cannot exist until it has given that owner back and drained.
            opening_publication_lost();
        }
        if control.phase() != LifecyclePhase::Opening {
            drop(ingress);
            drop(control);
            return self.reject_interrupted();
        }
        lifecycle.validate_open_attempt(&control, attempt)?;
        let ReadyForPublication { context, bundle } = self.0.complete();
        let receipt = match lifecycle.publish_generation_state(&mut control, attempt, bundle) {
            Ok(receipt) => receipt,
            Err(bundle) => {
                let refused =
                    PublicationReady(RollbackGuard::new(ReadyForPublication { context, bundle }));
                drop(ingress);
                drop(control);
                drop(refused);
                return Err(open_state_error());
            }
        };
        Ok(GenerationPublished {
            receipt,
            ingress,
            pending: RollbackGuard::new(PublishedOpen { context, control }),
        })
    }

    fn reject_interrupted(self) -> XllResult<GenerationPublished<'runtime, A>> {
        let (epoch, disposed) = self.0.complete().dispose_services();
        if !disposed {
            return Err(XllError::Panic);
        }
        let UnpublishedEpoch {
            context,
            module_epoch,
        } = epoch.complete();
        context.reject(|| module_epoch.begin_close(|| {}));
        Err(XllError::Closing)
    }
}

struct GenerationPublished<'runtime, A: crate::Addin> {
    receipt: CanonicalGenerationPublished<'runtime, A>,
    // Unlock ingress before the pending owner begins close on unwind.
    ingress: OpeningPublication<'static>,
    pending: RollbackGuard<PublishedOpen<'runtime, A>>,
}

struct PublishedOpen<'runtime, A: crate::Addin> {
    context: OpenCommitContext<'runtime, A>,
    control: LifecycleAccess<'runtime, A>,
}

impl<A: crate::Addin> Rollback for PublishedOpen<'_, A> {
    fn rollback(self) {
        let Self {
            context,
            mut control,
        } = self;
        context.deps.returns().close_admission();
        crate::module_runtime::ingress().begin_close_with(|| {});
        let lifecycle = context.deps.lifecycle_control();
        // If still Opening, resolve the abandoned attempt with its canonical
        // bundle intact. The epoch must never return through the opening owner.
        lifecycle.record_open_failure(&mut control);
        lifecycle.quarantine_state(&mut control);
        lifecycle.notify_all();
    }
}

struct OpenCommitted;

impl<A: crate::Addin> GenerationPublished<'_, A> {
    fn commit(self) -> XllResult<OpenCommitted> {
        let Self {
            mut pending,
            receipt,
            ingress,
        } = self;
        let PublishedOpen {
            context, control, ..
        } = pending.get_mut();
        let committed = context
            .deps
            .lifecycle_control()
            .finish_open_state(control, receipt)?;
        #[cfg(test)]
        check_open_commit_fault(5)?;
        context
            .deps
            .observer()
            .commit_open(&context.deps, &committed);
        committed
            .publish(ingress)
            .unwrap_or_else(|_| opening_publication_lost());
        let PublishedOpen { context, control } = pending.complete();
        drop(control);
        drop(context.binding);
        Ok(OpenCommitted)
    }
}

fn commit_inner<'runtime, A: crate::Addin>(
    deps: OpenDeps<'runtime, A>,
    core: PreparedOpenCore<A>,
    host: HostOpeningState,
    binding: InstalledLifecycle<'runtime, A>,
) -> XllResult<()> {
    let prepared = prepare_commit(deps, core, host, binding)?;
    #[cfg(test)]
    check_open_commit_fault(2)?;
    let ready = prepared.ready();
    #[cfg(test)]
    check_open_commit_fault(3)?;
    let published = ready.publish()?;
    #[cfg(test)]
    check_open_commit_fault(4)?;
    let OpenCommitted = published.commit()?;
    Ok(())
}

fn prepare_commit<'runtime, A: crate::Addin>(
    deps: OpenDeps<'runtime, A>,
    core: PreparedOpenCore<A>,
    host: HostOpeningState,
    binding: InstalledLifecycle<'runtime, A>,
) -> XllResult<ServicesPrepared<'runtime, A>> {
    let (attempt, module, service_inputs) = core.into_parts();
    let preparing = CommitHandoff(RollbackGuard::new(UncommittedModule {
        context: OpenCommitContext {
            deps,
            attempt,
            binding,
        },
        module,
    }));
    preparing.prepare(host, service_inputs)
}

fn open_state_error() -> XllError {
    crate::error::construct::internal(crate::diagnostics::catalog::OPEN_STATE)
}

#[cold]
fn opening_publication_lost() -> ! {
    crate::boundary::fail_stop_invariant(
        "xlAutoOpen opening publication",
        &crate::error::construct::internal(crate::diagnostics::catalog::OPEN_STATE),
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
    fn failed_service_preparation_makes_no_registration_host_calls() {
        use crate::registration::host::test_support::{CallbackScript, Reply};
        use crate::registration::{RegistrationDescriptor, RegistrationSignature};
        use xlfn_common::{ExecutionKind, FunctionVisibility, ReturnAbi};
        use xlfn_sys::{XL_GET_NAME, XLOPER12, XLOPER12Value, XLTYPE_STR};

        let runtime = Runtime::<()>::new();
        let path = if cfg!(windows) {
            "C:\\test.xll"
        } else {
            "/test.xll"
        };
        let mut module_name: Vec<u16> = path.encode_utf16().collect();
        module_name.insert(0, u16::try_from(module_name.len()).unwrap());
        // xlGetName and its result release are needed to build Addin::open's
        // context. Any callback after that bootstrap, including an event or
        // UDF registration, is unexpected.
        let script = CallbackScript::install([Reply::success(
            XL_GET_NAME,
            XLOPER12 {
                value: XLOPER12Value {
                    string: module_name.as_mut_ptr(),
                },
                xltype: XLTYPE_STR,
            },
        )]);
        let lifecycle = runtime.bind_addin_lifecycle().unwrap();
        let transaction = runtime
            .begin_open()
            .unwrap()
            .attach_host(HostCallbackSession::new());
        let descriptor = RegistrationDescriptor {
            export_name: "test_prepare_failure",
            excel_name: "TEST.PREPARE.FAILURE",
            signature: RegistrationSignature {
                execution: ExecutionKind::MainThread,
                return_abi: ReturnAbi::Value12,
                arguments: &[],
                volatile: false,
            },
            category: "Test",
            description: "",
            help_topic: "",
            visibility: FunctionVisibility::Public,
            arguments: &[],
        };
        FAIL_PREPARE_SERVICES.with(|fault| fault.set(true));
        let result = crate::runtime::transactions::open_addin_inner(
            crate::addin::BuildInfo::new(
                crate::diagnostics::AddinId::parse("prepare-failure").unwrap(),
                "0",
                "test",
            ),
            &[descriptor],
            transaction,
        );
        let Err(failure @ super::super::open::OpenFailure::Rollback { .. }) = result else {
            panic!("preparation failure must carry the opening rollback owner");
        };
        FAIL_PREPARE_SERVICES.with(|fault| assert!(!fault.get()));
        assert!(runtime.has_opening_generation());
        let _error = failure.recover(&lifecycle);
        assert_eq!(runtime.phase(), LifecyclePhase::Closed);
        assert!(!runtime.has_current_generation());
        assert_eq!(runtime.resident_event_count(), 0);
        script.assert_calls(&[XL_GET_NAME]);
    }

    #[test]
    fn installed_opening_retains_binding_until_commit_and_rollback_transfer() {
        let runtime = Runtime::<Probe>::new();
        let drops = Arc::new(AtomicUsize::new(0));
        let opening = runtime.begin_open().unwrap();
        let opening =
            runtime.publish_with_lifecycle(opening, (), RetainedLifecycle(Arc::clone(&drops)), ());
        let access = runtime.bind_addin_lifecycle().unwrap();
        assert!(matches!(
            runtime.addin_lifecycle.binding(&access),
            Err(ThreadAffineError::ReentrantAccess)
        ));
        let rollback = opening.into_rollback();
        assert!(matches!(
            runtime.addin_lifecycle.binding(&access),
            Err(ThreadAffineError::ReentrantAccess)
        ));
        rollback.run(&access);
        assert_eq!(runtime.phase(), LifecyclePhase::Closed);
        assert_eq!(drops.load(Ordering::SeqCst), 1);

        let opening = runtime.begin_open().unwrap();
        let opening = runtime
            .publish_with_lifecycle(opening, (), RetainedLifecycle(Arc::clone(&drops)), ())
            .stage_host_journal(HostMutationJournal::default());
        let access = runtime.bind_addin_lifecycle().unwrap();
        assert!(matches!(
            runtime.addin_lifecycle.binding(&access),
            Err(ThreadAffineError::ReentrantAccess)
        ));
        opening.commit().unwrap();
        // Normal running does not retain the exclusive occupancy claim.
        assert!(runtime.addin_lifecycle.binding(&access).is_ok());
        runtime
            .shutdown_deps()
            .take_addin_lifecycle(&access)
            .unwrap_or_else(|_| panic!("take"))
            .dispose()
            .unwrap_or_else(|_| panic!("dispose"))
            .release()
            .unwrap();
        assert_eq!(drops.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn acquisition_failure_retains_responsibility_before_and_after_module_begin() {
        for point in [1, 2, 3] {
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
                assert!(matches!(authority, ModuleCleanupClaim::NoModuleAuthority));
                assert!(runtime.lifecycle.access().module_epoch_id().is_none());
            } else {
                let ModuleCleanupClaim::Ready(ModuleCleanupReady::Closing(closing)) = authority
                else {
                    panic!("module authority lost during acquisition");
                };
                assert!(closing.id().is_current());
                runtime
                    .lifecycle_control()
                    .retain_failed_module_cleanup(ModuleCleanupAuthority::Closing(closing))
                    .unwrap_or_else(|_| panic!("canonical authority return refused"));
            }
        }
    }

    #[test]
    fn handoff_rejects_a_foreign_successor_and_retains_both_recoveries() {
        let runtime = Runtime::<()>::new();
        let other = Runtime::<()>::new();
        let successor = other.begin_open().unwrap();
        let start = runtime
            .lifecycle_control()
            .begin_open(RemovalEpoch::new(0))
            .unwrap();
        // Both runtimes allocate the same local attempt number. Identity must
        // be checked independently of the number and of the Rust lifetime.
        assert_eq!(start.attempt(), successor.attempt_id());
        let failure = match start.transfer_to_opening(successor) {
            Ok(_) => panic!("foreign successor accepted"),
            Err(failure) => failure,
        };
        assert_eq!(runtime.phase(), LifecyclePhase::Opening);
        assert_eq!(other.phase(), LifecyclePhase::Opening);
        // Failure owns the original locked start, then the unlocked successor.
        // Dropping it must not run successor rollback under the start lock.
        drop(failure);
        assert_eq!(runtime.phase(), LifecyclePhase::Quarantined);
        assert_eq!(other.phase(), LifecyclePhase::Quarantined);
        assert!(runtime.lifecycle.access().module_epoch_id().is_none());
        let ModuleCleanupClaim::Ready(ModuleCleanupReady::Closing(closing)) =
            other.lifecycle_control().take_module_cleanup_authority()
        else {
            panic!("failed handoff lost successor authority");
        };
        other
            .lifecycle_control()
            .retain_failed_module_cleanup(ModuleCleanupAuthority::Closing(closing))
            .unwrap_or_else(|_| panic!("canonical authority return refused"));
    }

    #[test]
    fn handoff_successor_owns_recovery_during_unwinding() {
        let runtime = Runtime::<()>::new();
        let result = crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(|| {
            let _successor = runtime.begin_open().unwrap();
            panic!("after opening responsibility handoff");
        }));
        assert!(result.is_err());
        assert_eq!(runtime.phase(), LifecyclePhase::Quarantined);
        assert!(runtime.return_protocol.returns_closed_and_quiescent());
        let ModuleCleanupClaim::Ready(ModuleCleanupReady::Closing(closing)) =
            runtime.lifecycle_control().take_module_cleanup_authority()
        else {
            panic!("successor unwinding lost authority");
        };
        runtime
            .lifecycle_control()
            .retain_failed_module_cleanup(ModuleCleanupAuthority::Closing(closing))
            .unwrap_or_else(|_| panic!("canonical authority return refused"));
    }

    #[test]
    fn miri_commit_stage_failure_keeps_the_single_module_owner() {
        for point in 1..=5 {
            for unwind in [false, true] {
                let runtime = Runtime::<()>::new();
                let opening = runtime.begin_open().unwrap();
                let opening = runtime
                    .publish(opening, (), ())
                    .stage_host_journal(HostMutationJournal::default());
                FAIL_OPEN_COMMIT.with(|fault| fault.set((point, unwind)));
                let failed =
                    crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(|| {
                        opening.commit()
                    }));
                if unwind {
                    assert!(failed.is_err());
                } else {
                    assert!(matches!(failed, Ok(Err(XllError::Closing))));
                }
                FAIL_OPEN_COMMIT.with(|fault| assert_eq!(fault.get(), (0, false)));
                let control = runtime.lifecycle.access();
                assert_eq!(control.phase(), LifecyclePhase::Quarantined);
                assert_eq!(control.open_attempt(), None);
                assert_eq!(control.has_current_generation(), point >= 4);
                assert_eq!(control.last_committed_generation(), None);
                let epoch = control
                    .module_epoch_id()
                    .expect("quarantine owns the module");
                assert!(epoch.is_current());
                drop(control);
                assert!(runtime.begin_open().is_err());
                assert!(runtime.return_protocol.returns_closed_and_quiescent());
                assert_eq!(
                    crate::module_runtime::ingress().phase(),
                    crate::ingress::PHASE_CLOSING
                );
                let ModuleCleanupClaim::Ready(ModuleCleanupReady::Closing(closing)) =
                    runtime.lifecycle_control().take_module_cleanup_authority()
                else {
                    panic!("failed publication lost its cleanup owner");
                };
                assert_eq!(closing.id(), epoch);
                runtime
                    .lifecycle_control()
                    .retain_failed_module_cleanup(ModuleCleanupAuthority::Closing(closing))
                    .unwrap_or_else(|_| panic!("canonical authority return refused"));
            }
        }
    }

    #[test]
    fn rejected_publication_preserves_the_last_successful_generation() {
        let runtime = Runtime::<()>::new();
        let opening = runtime.begin_open().unwrap();
        let opening = runtime.publish(opening, (), ());
        runtime.finish_open(opening, Vec::new()).unwrap();
        let first = runtime.last_committed_generation().unwrap();
        let lifecycle = runtime.bind_addin_lifecycle().unwrap();
        assert_eq!(
            crate::runtime::test_support::remove_addin(
                &runtime,
                &lifecycle,
                &mut HostCallbackSession::new(),
            ),
            1,
        );
        assert_eq!(runtime.phase(), LifecyclePhase::Closed);
        runtime.clear_host_intent();
        let opening = runtime.begin_open().unwrap();
        let opening = runtime
            .publish(opening, (), ())
            .stage_host_journal(HostMutationJournal::default());
        FAIL_OPEN_COMMIT.with(|fault| fault.set((5, false)));
        assert!(matches!(opening.commit(), Err(XllError::Closing)));
        assert_eq!(runtime.phase(), LifecyclePhase::Quarantined);
        assert_eq!(runtime.last_committed_generation(), Some(first));
    }

    #[test]
    fn miri_publication_rejection_preserves_the_unpublished_epoch() {
        let runtime = Runtime::<()>::new();
        let other = Runtime::<()>::new();
        let opening = runtime.begin_open().unwrap();
        let opening = runtime
            .publish(opening, (), ())
            .stage_host_journal(HostMutationJournal::default());
        let (deps, HostMutated { core, host }, binding) = opening.into_parts();
        let attempt = core.attempt_id();
        let ready = prepare_commit(deps, core, host, binding).unwrap().ready();
        let ReadyForPublication { context, bundle } = ready.0.complete();
        let other_lifecycle = other.lifecycle_control();
        let mut access = other_lifecycle.access();
        let bundle = deps
            .lifecycle_control()
            .publish_generation_state(&mut access, attempt, bundle)
            .err()
            .expect("foreign lifecycle access cannot receive this bundle");
        let epoch = bundle.module_epoch.id();
        assert_eq!(access.phase(), LifecyclePhase::Closed);
        assert!(access.module_epoch_id().is_none());
        drop(access);
        drop(PublicationReady(RollbackGuard::new(ReadyForPublication {
            context,
            bundle,
        })));
        let control = runtime.lifecycle.access();
        assert_eq!(control.phase(), LifecyclePhase::Quarantined);
        assert_eq!(control.module_epoch_id(), Some(epoch));
        assert!(!control.has_current_generation());
        assert!(control.has_opening_generation());
    }

    #[test]
    #[cfg_attr(miri, ignore = "requires a child process to observe fail-stop")]
    fn receiver_transfer_panic_cannot_return_to_the_open_boundary() {
        const CHILD: &str = "XLFN_TEST_OPEN_RECEIVER_TRANSFER";
        if std::env::var_os(CHILD).is_some() {
            let runtime = Runtime::<()>::new();
            let opening = runtime.begin_open().unwrap();
            let opening = runtime
                .publish(opening, (), ())
                .stage_host_journal(HostMutationJournal::default());
            crate::lifecycle::LifecycleCoordinator::<()>::inject_publication_transfer_panic();
            let _ = crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(|| {
                opening.commit()
            }));
            panic!("receiver fault returned to outer open fallback");
        }
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "runtime::open_txn::owner_tests::receiver_transfer_panic_cannot_return_to_the_open_boundary"])
            .env(CHILD, "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(status.signal(), Some(6), "expected fail-stop SIGABRT");
        }
        #[cfg(not(unix))]
        {
            assert!(!status.success());
            assert_ne!(
                status.code(),
                Some(101),
                "expected fail-stop before fallback"
            );
        }
    }

    #[test]
    #[cfg(all(feature = "rtd", any(feature = "handles", not(target_os = "windows"))))]
    fn miri_close_cannot_claim_opening_during_unpublished_service_destruction() {
        use crate::subscription::{RtdSink, RtdSource, RtdTopic, SourceRegistration};

        struct ReenterOnDrop {
            runtime: std::sync::Weak<Runtime<()>>,
            attempt: OpenAttemptId,
            drops: Arc<AtomicUsize>,
        }
        impl Drop for ReenterOnDrop {
            fn drop(&mut self) {
                let runtime = self.runtime.upgrade().unwrap();
                let mut control = runtime.lifecycle.access();
                assert_eq!(control.phase(), LifecyclePhase::Closing);
                assert_eq!(control.open_attempt(), Some(self.attempt));
                let crate::lifecycle::FinalCloseDecision::Closing(closing) =
                    control.final_close_decision()
                else {
                    panic!("close waiter lost its pending state");
                };
                assert!(closing.try_claim().is_none());
                self.drops.fetch_add(1, Ordering::SeqCst);
            }
        }
        // SAFETY: this source starts no work and never stores a sink.
        #[allow(unsafe_code, reason = "test source never retains a subscription sink")]
        unsafe impl RtdSource for ReenterOnDrop {
            type Value = i32;
            type Subscription = ();
            fn subscribe(&self, _: &RtdTopic, _: RtdSink<i32>) -> XllResult<()> {
                Err(XllError::Closing)
            }
        }

        let runtime = Arc::new(Runtime::<()>::new());
        let opening = runtime.begin_open().unwrap();
        let attempt = opening.attempt_id();
        let registration = SourceRegistration::new(attempt.into_runtime_generation());
        let drops = Arc::new(AtomicUsize::new(0));
        let _source = registration
            .register(ReenterOnDrop {
                runtime: Arc::downgrade(&runtime),
                attempt,
                drops: Arc::clone(&drops),
            })
            .unwrap();
        let opening = runtime
            .publish_with_sources(opening, (), (), registration.finish())
            .stage_host_journal(HostMutationJournal::default());
        let (deps, HostMutated { core, host }, binding) = opening.into_parts();
        let ready = prepare_commit(deps, core, host, binding).unwrap().ready();
        runtime.lifecycle.access().request_closing_for_test();
        assert!(matches!(ready.publish(), Err(XllError::Closing)));
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        let control = runtime.lifecycle.access();
        assert_eq!(control.phase(), LifecyclePhase::Closing);
        assert_eq!(control.open_attempt(), None);
        assert!(control.module_epoch_id().is_some());
    }

    #[test]
    #[cfg(all(feature = "rtd", any(feature = "handles", not(target_os = "windows"))))]
    fn miri_rollback_keeps_module_authority_until_sources_are_disposed() {
        use crate::subscription::{RtdSink, RtdSource, RtdTopic, SourceRegistration};

        struct ReenterOnDrop {
            runtime: std::sync::Weak<Runtime<()>>,
            attempt: OpenAttemptId,
            drops: Arc<AtomicUsize>,
            panic: bool,
        }
        impl Drop for ReenterOnDrop {
            fn drop(&mut self) {
                let runtime = self.runtime.upgrade().unwrap();
                let mut control = runtime.lifecycle.access();
                assert_eq!(control.phase(), LifecyclePhase::Closing);
                assert_eq!(control.open_attempt(), Some(self.attempt));
                let crate::lifecycle::FinalCloseDecision::Closing(closing) =
                    control.final_close_decision()
                else {
                    panic!("close waiter lost its pending state");
                };
                assert!(closing.try_claim().is_none());
                self.drops.fetch_add(1, Ordering::SeqCst);
                assert!(!self.panic, "injected source destructor panic");
            }
        }
        // SAFETY: this source starts no work and never retains a sink.
        #[allow(unsafe_code, reason = "test source never retains a subscription sink")]
        unsafe impl RtdSource for ReenterOnDrop {
            type Value = i32;
            type Subscription = ();
            fn subscribe(&self, _: &RtdTopic, _: RtdSink<i32>) -> XllResult<()> {
                Err(XllError::Closing)
            }
        }

        for rollback in [false, true] {
            for panic in [false, true] {
                let runtime = Arc::new(Runtime::<()>::new());
                let opening = runtime.begin_open().unwrap();
                let attempt = opening.attempt_id();
                let registration = SourceRegistration::new(attempt.into_runtime_generation());
                let drops = Arc::new(AtomicUsize::new(0));
                let _source = registration
                    .register(ReenterOnDrop {
                        runtime: Arc::downgrade(&runtime),
                        attempt,
                        drops: Arc::clone(&drops),
                        panic,
                    })
                    .unwrap();
                let opening = runtime.publish_with_sources(opening, (), (), registration.finish());
                runtime.lifecycle.access().request_closing_for_test();
                if rollback {
                    let lifecycle = runtime.bind_addin_lifecycle().unwrap();
                    opening.into_rollback().run(&lifecycle);
                } else {
                    drop(opening);
                }
                assert_eq!(drops.load(Ordering::SeqCst), 1);
                assert_eq!(
                    runtime.phase(),
                    if rollback && !panic {
                        LifecyclePhase::Closing
                    } else {
                        LifecyclePhase::Quarantined
                    }
                );
            }
        }
    }

    #[test]
    #[allow(
        clippy::mem_forget,
        reason = "a forgotten publication cannot certify success"
    )]
    fn forgotten_ready_publication_cannot_certify_open() {
        let fixture = crate::runtime::StaticTestRuntime::<()>::new();
        let runtime = fixture.runtime();
        let opening = runtime.begin_open().unwrap();
        let opening = runtime
            .publish(opening, (), ())
            .stage_host_journal(HostMutationJournal::default());
        let (deps, HostMutated { core, host }, binding) = opening.into_parts();
        let attempt = core.attempt_id();
        let ready = prepare_commit(deps, core, host, binding).unwrap().ready();
        std::mem::forget(ready);
        let control = runtime.lifecycle.access();
        assert_eq!(control.phase(), LifecyclePhase::Opening);
        assert_eq!(control.open_attempt(), Some(attempt));
        assert_eq!(control.last_committed_generation(), None);
        assert!(!control.has_current_generation());
        assert_eq!(
            crate::module_runtime::ingress().phase(),
            crate::ingress::PHASE_OPENING
        );
        drop(control);
        assert!(runtime.begin_open().is_err());
        drop(fixture);
    }

    #[test]
    #[allow(
        clippy::mem_forget,
        reason = "forgetting a successor must not certify or reclaim an unfinished opening"
    )]
    fn forgotten_handoff_successor_leaves_the_attempt_incomplete() {
        // Native-only: the deliberately forgotten module owner and its runtime
        // remain alive together. The fixture releases only test infrastructure
        // so the leaked module test lease cannot block the next libtest thread.
        // This test does not qualify leak-free cleanup.
        let fixture = crate::runtime::StaticTestRuntime::<()>::new();
        let runtime = fixture.runtime();
        let successor = runtime.begin_open().unwrap();
        let attempt = successor.attempt_id();
        std::mem::forget(successor);
        let control = runtime.lifecycle.access();
        assert_eq!(control.phase(), LifecyclePhase::Opening);
        assert_eq!(control.open_attempt(), Some(attempt));
        assert!(!control.has_current_generation());
        assert!(!control.has_opening_generation());
        drop(control);
        assert!(runtime.begin_open().is_err());
        assert_eq!(runtime.phase(), LifecyclePhase::Opening);
        drop(fixture);
        assert_eq!(runtime.phase(), LifecyclePhase::Opening);
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
    fn removal_during_initialization_retains_the_shared_root_for_cleanup() {
        let runtime = Runtime::<Probe>::new();
        let drops = Arc::new(AtomicUsize::new(0));
        let transaction = initialized(&runtime, &drops);
        runtime.lifecycle.access().request_closing_for_test();
        let failure = match transaction.stage_generation(OpeningGeneration {
            shared_state: (),
            layers: (),
            init_config: crate::addin_impl::RuntimeConfig::default(),
        }) {
            Ok(_) => panic!("closing attempt proceeded to publication"),
            Err(failure) => failure,
        };
        assert!(runtime.has_opening_generation());
        let lifecycle = runtime.bind_addin_lifecycle().unwrap();
        assert!(matches!(failure.recover(&lifecycle), XllError::Closing));
        assert_eq!(runtime.phase(), LifecyclePhase::Closing);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        assert_eq!(crate::boundary::host::host_auto_remove(&runtime), 1);
        assert_eq!(runtime.phase(), LifecyclePhase::Closed);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(runtime.quarantine_snapshot().is_empty());
    }

    #[test]
    fn abandoned_inputs_are_disposed_before_module_return_even_on_panic() {
        struct Inputs<'a> {
            runtime: &'a Runtime<()>,
            observed: &'a std::cell::Cell<bool>,
            panic: bool,
        }
        impl Drop for Inputs<'_> {
            fn drop(&mut self) {
                // Reentry must run outside the canonical lock, while the
                // unique opening transaction still excludes final removal.
                assert_eq!(self.runtime.phase(), LifecyclePhase::Opening);
                assert!(self.runtime.lifecycle.access().open_attempt().is_some());
                self.observed.set(true);
                assert!(!self.panic, "injected source catalog destructor panic");
            }
        }
        for panic in [false, true] {
            let runtime = Runtime::<()>::new();
            let observed = std::cell::Cell::new(false);
            let transaction = runtime.begin_open().unwrap();
            let (deps, Begun { core }, ()) = transaction.into_parts();
            let core = core.map_inputs(|()| Inputs {
                runtime: &runtime,
                observed: &observed,
                panic,
            });
            abandon_open(&deps, core, LifecycleOwnership::Uninstalled);
            assert!(observed.get());
            assert_eq!(runtime.phase(), LifecyclePhase::Quarantined);
        }
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
            .retain_failed_module_cleanup(ModuleCleanupAuthority::Closing(closing))
            .unwrap_or_else(|_| panic!("canonical authority return refused"));
    }

    #[test]
    fn rollback_unwind_on_either_side_of_module_transfer_keeps_original_authority() {
        for point in [1, 2] {
            let runtime = Runtime::<Probe>::new();
            let other = Runtime::<Probe>::new();
            let drops = Arc::new(AtomicUsize::new(0));
            let rollback = initialized(&runtime, &drops).into_rollback();
            let epoch = crate::module_runtime::ingress().epoch();
            let lifecycle = runtime.bind_addin_lifecycle().unwrap();
            PANIC_ROLLBACK_TRANSFER.with(|fault| fault.set(point));
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
                .retain_failed_module_cleanup(ModuleCleanupAuthority::Closing(closing))
                .unwrap_or_else(|_| panic!("canonical authority return refused"));
        }
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
