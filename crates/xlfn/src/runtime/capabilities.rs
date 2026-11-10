//! Operation-scoped views assembled by [`super::Runtime`].
//!
//! These views are deliberately borrowed and private to the runtime domain.
//! They make the facilities used by an operation explicit without turning
//! `Runtime` itself into a service locator available to every transaction.

use super::{AddinLifecycleAccess, Runtime};
use crate::addin_impl::Addin;
use crate::lifecycle::{LifecycleAccess, LifecycleControl, LifecycleCoordinator};
use crate::runtime::observer::RuntimeObserver;
use crate::runtime_components::{
    GenerationServices, HostLedger, ModuleResidency, QuarantineVault, ReturnProtocol,
};
use xlfn_kernel::thread_affine::{
    BindingEmpty, BindingOccupied, BindingState, BindingTxn, ThreadAffineError,
    ThreadAffineInstallError, ThreadAffineSlot,
};

/// Occupancy authority retained from installation through commit or rollback.
#[must_use = "retain lifecycle installation authority through commit or rollback"]
pub(crate) struct InstalledLifecycle<'runtime, A: Addin> {
    binding: BindingTxn<'runtime, A::LifecycleState, BindingOccupied>,
}

/// The extracted value and its exact empty binding remain one responsibility.
#[must_use = "dispose the lifecycle before releasing its exact binding"]
pub(crate) struct TakenLifecycle<'runtime, A: Addin> {
    // Field order retains the claim through ordinary Drop and unwinding of value.
    value: A::LifecycleState,
    empty: EmptyLifecycle<'runtime, A>,
}

#[must_use = "retain the empty binding until terminal release"]
pub(crate) struct EmptyLifecycle<'runtime, A: Addin> {
    binding: BindingTxn<'runtime, A::LifecycleState, BindingEmpty>,
}

/// A terminal failure retains the precise phase already reached. In particular,
/// disposal panic retains only the empty binding, never an already-dropped value.
pub(crate) struct LifecycleFailure<'runtime, A: Addin> {
    error: crate::XllError,
    _authority: RetainedLifecycle<'runtime, A>,
}
#[allow(
    dead_code,
    reason = "retained phase fields own rollback obligations until quarantine"
)]
enum RetainedLifecycle<'runtime, A: Addin> {
    None,
    Occupied(InstalledLifecycle<'runtime, A>),
    Empty(EmptyLifecycle<'runtime, A>),
}
impl<A: Addin> std::fmt::Debug for LifecycleFailure<'_, A> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LifecycleFailure")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}
impl<'runtime, A: Addin> LifecycleFailure<'runtime, A> {
    pub(in crate::runtime) fn error(&self) -> &crate::XllError {
        &self.error
    }
    pub(in crate::runtime) fn disposed(
        error: crate::XllError,
        empty: EmptyLifecycle<'runtime, A>,
    ) -> Self {
        Self {
            error,
            _authority: RetainedLifecycle::Empty(empty),
        }
    }
    pub(in crate::runtime) fn access(reason: ThreadAffineError) -> Self {
        Self {
            error: crate::lifecycle::lifecycle_access_error(reason),
            _authority: RetainedLifecycle::None,
        }
    }
    pub(in crate::runtime) fn retain(
        error: crate::XllError,
        binding: Option<InstalledLifecycle<'runtime, A>>,
    ) -> Self {
        Self {
            error,
            _authority: binding.map_or(RetainedLifecycle::None, RetainedLifecycle::Occupied),
        }
    }
}
impl<'runtime, A: Addin> InstalledLifecycle<'runtime, A> {
    pub(in crate::runtime) fn take(
        self,
    ) -> Result<TakenLifecycle<'runtime, A>, LifecycleFailure<'runtime, A>> {
        match self.binding.take() {
            Ok((value, binding)) => Ok(TakenLifecycle {
                value,
                empty: EmptyLifecycle { binding },
            }),
            Err(error) => Err(LifecycleFailure {
                error: crate::lifecycle::lifecycle_access_error(error.reason),
                _authority: RetainedLifecycle::Occupied(Self {
                    binding: error.transaction,
                }),
            }),
        }
    }
}
impl<'runtime, A: Addin> TakenLifecycle<'runtime, A> {
    pub(in crate::runtime) fn dispose(
        self,
    ) -> Result<EmptyLifecycle<'runtime, A>, LifecycleFailure<'runtime, A>> {
        let Self { value, empty } = self;
        match crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(|| drop(value))) {
            Ok(()) => Ok(empty),
            Err(_) => Err(LifecycleFailure {
                error: crate::XllError::Panic,
                _authority: RetainedLifecycle::Empty(empty),
            }),
        }
    }
}
impl<'runtime, A: Addin> EmptyLifecycle<'runtime, A> {
    pub(in crate::runtime) fn release(self) -> Result<(), LifecycleFailure<'runtime, A>> {
        #[cfg(test)]
        PANIC_BINDING_RELEASE
            .with(|fail| assert!(!fail.replace(false), "injected binding release unwind"));
        #[cfg(test)]
        if FAIL_BINDING_RELEASE.with(|fail| fail.replace(false)) {
            return Err(LifecycleFailure::disposed(crate::XllError::Panic, self));
        }
        match self.binding.release() {
            Ok(_) => Ok(()),
            Err(error) => Err(LifecycleFailure {
                error: crate::lifecycle::lifecycle_access_error(error.reason),
                _authority: RetainedLifecycle::Empty(Self {
                    binding: error.transaction,
                }),
            }),
        }
    }
}

/// Created from an opening's own dependencies, after issuer and attempt checks.
/// Its dispatch target is carried inside; callers cannot supply another runtime.
pub(crate) struct RollbackRequest<'runtime, A: Addin> {
    deps: OpenDeps<'runtime, A>,
    generation: crate::generation::RuntimeGeneration,
    installed: Option<InstalledLifecycle<'runtime, A>>,
}
impl<'runtime, A: Addin> RollbackRequest<'runtime, A> {
    pub(in crate::runtime) fn run(
        self,
        lifecycle: &AddinLifecycleAccess<'_, A>,
        callbacks: &mut crate::host_callback::HostCallbackSession,
    ) -> super::rollback::OpenRollbackOutcome {
        let recovery = self.deps.recovery();
        recovery.target.rollback(lifecycle, callbacks, self)
    }
    pub(in crate::runtime) fn generation(&self) -> crate::generation::RuntimeGeneration {
        self.generation
    }
    pub(in crate::runtime) fn matches(&self, deps: ShutdownDeps<'_, A>) -> bool {
        std::ptr::eq(self.deps.lifecycle, deps.lifecycle)
            && deps
                .lifecycle
                .access()
                .matches_latest_open_attempt(self.generation)
            && self
                .installed
                .as_ref()
                .is_none_or(|installed| installed.binding.belongs_to(deps.addin_lifecycle))
    }
    pub(in crate::runtime) fn inherit(
        self,
        deps: ShutdownDeps<'_, A>,
    ) -> Result<Option<InstalledLifecycle<'runtime, A>>, LifecycleFailure<'runtime, A>> {
        if !self.matches(deps) {
            return Err(LifecycleFailure::retain(
                crate::lifecycle::lifecycle_access_error(ThreadAffineError::StaleAccess),
                self.installed,
            ));
        }
        Ok(self.installed)
    }
}

#[cfg(test)]
thread_local! {
    pub(in crate::runtime) static PANIC_BINDING_RELEASE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    pub(in crate::runtime) static FAIL_BINDING_RELEASE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Source-bound recovery operations. The target is installed only by the
/// composition root and cannot be replaced while an opening owner is alive.
pub(crate) struct OpenRecovery<'a, A: Addin> {
    target: &'a dyn OpenRecoveryTarget<A>,
}

impl<A: Addin> Copy for OpenRecovery<'_, A> {}
impl<A: Addin> Clone for OpenRecovery<'_, A> {
    fn clone(&self) -> Self {
        *self
    }
}

trait OpenRecoveryTarget<A: Addin>: Sync {
    fn rollback(
        &self,
        lifecycle: &AddinLifecycleAccess<'_, A>,
        callbacks: &mut crate::host_callback::HostCallbackSession,
        request: RollbackRequest<'_, A>,
    ) -> super::rollback::OpenRollbackOutcome;
    fn quarantine(&self, hazard: Option<crate::shutdown::UnloadHazard>);
}

impl<A: Addin, P: super::UnloadPolicy<A>> OpenRecoveryTarget<A> for Runtime<A, P> {
    fn rollback(
        &self,
        lifecycle: &AddinLifecycleAccess<'_, A>,
        callbacks: &mut crate::host_callback::HostCallbackSession,
        request: RollbackRequest<'_, A>,
    ) -> super::rollback::OpenRollbackOutcome {
        super::rollback::rollback_open_request(self, lifecycle, callbacks, request)
    }
    fn quarantine(&self, hazard: Option<crate::shutdown::UnloadHazard>) {
        match hazard {
            Some(hazard) => super::recovery::quarantine_for_hazard(self, hazard),
            None => super::recovery::quarantine_runtime(self),
        }
    }
}

impl<A: Addin> OpenRecovery<'_, A> {
    pub(in crate::runtime) fn quarantine(&self, hazard: Option<crate::shutdown::UnloadHazard>) {
        self.target.quarantine(hazard);
    }
}

/// Facilities needed by one open transaction.
///
/// The fields remain private so callers can only use the operation-specific
/// methods below. In particular, adding a new runtime-wide dependency to an
/// open transaction requires an explicit capability API change.
pub(crate) struct OpenDeps<'a, A: Addin> {
    lifecycle: &'a LifecycleCoordinator<A>,
    addin_lifecycle: &'a ThreadAffineSlot<A::LifecycleState>,
    host: &'a HostLedger,
    residency: &'a ModuleResidency,
    returns: &'a ReturnProtocol,
    #[cfg(feature = "async")]
    async_runtime: &'a crate::async_udf::AsyncRuntime<A::AsyncExecutor>,
    quarantine: &'a QuarantineVault<A>,
    observer: &'a RuntimeObserver,
    recovery: OpenRecovery<'a, A>,
}

impl<A: Addin> Copy for OpenDeps<'_, A> {}

impl<A: Addin> Clone for OpenDeps<'_, A> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<'a, A: Addin> OpenDeps<'a, A> {
    pub(in crate::runtime) const fn from_runtime(
        runtime: &'a Runtime<A, impl crate::runtime::UnloadPolicy<A>>,
    ) -> Self {
        Self {
            lifecycle: &runtime.lifecycle,
            addin_lifecycle: &runtime.addin_lifecycle,
            host: &runtime.host,
            residency: &runtime.residency,
            returns: &runtime.return_protocol,
            #[cfg(feature = "async")]
            async_runtime: &runtime.async_runtime,
            quarantine: &runtime.quarantine,
            observer: &runtime.observer,
            recovery: OpenRecovery { target: runtime },
        }
    }

    pub(in crate::runtime) fn rollback_request(
        &self,
        attempt: crate::generation::OpenAttemptId,
        installed: Option<InstalledLifecycle<'a, A>>,
    ) -> Result<RollbackRequest<'a, A>, LifecycleFailure<'a, A>> {
        if self.lifecycle.access().open_attempt() != Some(attempt)
            || installed
                .as_ref()
                .is_some_and(|binding| !binding.binding.belongs_to(self.addin_lifecycle))
        {
            return Err(LifecycleFailure::retain(
                crate::lifecycle::lifecycle_access_error(ThreadAffineError::StaleAccess),
                installed,
            ));
        }
        Ok(RollbackRequest {
            deps: *self,
            generation: attempt.into_runtime_generation(),
            installed,
        })
    }

    pub(in crate::runtime) fn recovery(&self) -> OpenRecovery<'a, A> {
        self.recovery
    }

    pub(in crate::runtime) fn lifecycle(&self) -> &'a LifecycleCoordinator<A> {
        self.lifecycle
    }

    pub(in crate::runtime) fn lifecycle_control(&self) -> LifecycleControl<'_, A> {
        LifecycleControl::new(self.lifecycle)
    }

    pub(in crate::runtime) fn lifecycle_access(&self) -> LifecycleAccess<'_, A> {
        self.lifecycle.access()
    }

    pub(in crate::runtime) fn returns(&self) -> &'a ReturnProtocol {
        self.returns
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(in crate::runtime) fn host(&self) -> &'a HostLedger {
        self.host
    }

    #[cfg(feature = "async")]
    pub(in crate::runtime) fn async_runtime(
        &self,
    ) -> &'a crate::async_udf::AsyncRuntime<A::AsyncExecutor> {
        self.async_runtime
    }

    pub(in crate::runtime) fn quarantine(&self) -> &'a QuarantineVault<A> {
        self.quarantine
    }

    pub(in crate::runtime) fn quarantine_opening_generation(
        &self,
        generation: Option<crate::generation::RuntimeGeneration>,
        opening: crate::generation::OpeningGeneration<A>,
        reason: crate::runtime_components::QuarantineReason,
    ) {
        let crate::generation::OpeningGeneration {
            shared_state,
            layers,
            init_config: _,
        } = opening;
        if let Some(id) = generation {
            self.quarantine.retain_generation(
                Some(id),
                crate::generation::ExecutionGeneration {
                    id,
                    shared_state,
                    layers,
                },
                reason,
            );
        } else {
            self.quarantine
                .retain_shared_state(None, shared_state, reason);
            self.quarantine.retain_layers(None, layers, reason);
        }
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(in crate::runtime) fn residency(&self) -> &'a ModuleResidency {
        self.residency
    }

    pub(in crate::runtime) fn observer(&self) -> &'a RuntimeObserver {
        self.observer
    }

    pub(in crate::runtime) fn protocol_generation(
        &self,
    ) -> Option<crate::generation::RuntimeGeneration> {
        self.lifecycle.access().protocol_generation()
    }

    pub(in crate::runtime) fn host_intent(&self) -> crate::lifecycle::HostLifecycleIntent {
        self.lifecycle.access().host_intent()
    }

    pub(in crate::runtime) fn ensure_calculation_events(
        &self,
        host: &crate::registration::RegistrationHost<'_>,
        has_async_functions: bool,
    ) -> crate::XllResult<()> {
        self.residency
            .ensure_calculation_events(host, has_async_functions)
    }

    pub(in crate::runtime) fn merge_host(&self, journal: crate::registration::HostMutationJournal) {
        self.host.merge(journal);
    }

    pub(in crate::runtime) fn install_addin_lifecycle(
        &self,
        access: &AddinLifecycleAccess<'_, A>,
        state: A::LifecycleState,
    ) -> Result<InstalledLifecycle<'a, A>, ThreadAffineInstallError<A::LifecycleState>> {
        match self.addin_lifecycle.binding(access) {
            Ok(BindingState::Empty(transaction)) => transaction
                .install(state)
                .map(|binding| InstalledLifecycle { binding })
                .map_err(|error| ThreadAffineInstallError {
                    value: error.value,
                    reason: error.reason,
                }),
            Ok(BindingState::Occupied(_transaction)) => Err(ThreadAffineInstallError {
                value: state,
                reason: ThreadAffineError::Occupied,
            }),
            Err(reason) => Err(ThreadAffineInstallError {
                value: state,
                reason,
            }),
        }
    }
}

/// Facilities needed by shutdown and removal transactions.
///
/// Shutdown stages receive this view instead of the aggregate `Runtime`, so
/// they can only operate on lifecycle state, host/return resources, the
/// optional async executor, quarantine, and the refinement observer. The
/// composition root remains the only place that assembles this set.
pub(crate) struct ShutdownDeps<'a, A: Addin> {
    lifecycle: &'a LifecycleCoordinator<A>,
    addin_lifecycle: &'a ThreadAffineSlot<A::LifecycleState>,
    host: &'a HostLedger,
    residency: &'a ModuleResidency,
    returns: &'a ReturnProtocol,
    #[cfg(feature = "async")]
    async_runtime: &'a crate::async_udf::AsyncRuntime<A::AsyncExecutor>,
    quarantine: &'a QuarantineVault<A>,
    observer: &'a RuntimeObserver,
}

impl<A: Addin> Copy for ShutdownDeps<'_, A> {}

impl<A: Addin> Clone for ShutdownDeps<'_, A> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<'a, A: Addin> ShutdownDeps<'a, A> {
    pub(in crate::runtime) fn from_runtime(
        runtime: &'a Runtime<A, impl crate::runtime::UnloadPolicy<A>>,
    ) -> Self {
        Self {
            lifecycle: &runtime.lifecycle,
            addin_lifecycle: &runtime.addin_lifecycle,
            host: &runtime.host,
            residency: &runtime.residency,
            returns: &runtime.return_protocol,
            #[cfg(feature = "async")]
            async_runtime: &runtime.async_runtime,
            quarantine: &runtime.quarantine,
            observer: &runtime.observer,
        }
    }

    pub(in crate::runtime) fn returns(&self) -> &'a ReturnProtocol {
        self.returns
    }

    pub(in crate::runtime) fn lifecycle(&self) -> &'a LifecycleCoordinator<A> {
        self.lifecycle
    }

    pub(in crate::runtime) fn lifecycle_control(&self) -> LifecycleControl<'_, A> {
        LifecycleControl::new(self.lifecycle)
    }

    pub(in crate::runtime) fn host(&self) -> &'a HostLedger {
        self.host
    }

    #[cfg(feature = "async")]
    pub(in crate::runtime) fn async_runtime(
        &self,
    ) -> &'a crate::async_udf::AsyncRuntime<A::AsyncExecutor> {
        self.async_runtime
    }

    pub(in crate::runtime) fn quarantine(&self) -> &'a QuarantineVault<A> {
        self.quarantine
    }

    pub(in crate::runtime) fn residency(&self) -> &'a ModuleResidency {
        self.residency
    }

    pub(in crate::runtime) fn observer(&self) -> &'a RuntimeObserver {
        self.observer
    }

    /// Observe the terminal state while its lifecycle publication is locked.
    /// The affine proof supplies the drained subsystem facts for counters that
    /// are not retained after their generation services have been consumed.
    #[cfg(any(test, feature = "refinement"))]
    pub(in crate::runtime) fn terminal_resources(
        &self,
        control: &crate::lifecycle::LifecycleAccess<'_, A>,
        _proof: &crate::runtime::shutdown::QuiescenceProof,
    ) -> crate::shutdown_trace::ShutdownResources {
        let mut resources = crate::shutdown_trace::ShutdownResources::quiescent_snapshot();
        let ingress = crate::module_runtime::ingress();
        resources.ingress_open = ingress.phase() == crate::ingress::PHASE_OPEN;
        resources.active_calls = ingress.active_udfs() as u64;
        // external_entries remains the proof's drained-export projection:
        // raw export reservations can include the current removal boundary,
        // rather than another generation-owned activity awaiting teardown.
        let (registrations, registration_state_known) = self.host.registration_resources();
        resources.registrations = registrations as u64;
        resources.resident_event_registrations = self.residency.resident_event_count() as u64;
        resources.module_residency_held = self.residency.is_held();
        resources.registration_state_known = registration_state_known;
        let callbacks = crate::module_runtime::global().callback_admission();
        resources.callback_gate_open =
            callbacks.lifecycle() == crate::callback_gate::ModuleCallbackLifecycle::Open;
        // The proof has consumed subscription callbacks. Retained host callback
        // reservations must also prevent a falsely clean terminal observation.
        resources.callbacks = callbacks.active() as u64;
        resources.return_blocks = self.returns.outstanding_obligations() as u64;
        #[cfg(feature = "async")]
        {
            resources.async_executor_running = !self.async_runtime.is_stopped();
        }
        resources.generation_owned_by_runtime =
            control.has_current_generation() || control.has_opening_generation();
        resources.generation_unique = !resources.generation_owned_by_runtime;
        // A logical teardown certificate does not consume externally owned
        // COM objects. Preserve their physical ownership in terminal evidence.
        let rtd = crate::excel_rtd_protocol::trace_snapshot();
        resources.rtd_class_factories = rtd.rtd_class_factories;
        resources.rtd_servers = rtd.rtd_servers;
        resources.rtd_server_locks = rtd.rtd_server_locks;
        let diagnostics = crate::diagnostics::trace_snapshot();
        resources.diagnostics_running = diagnostics.running;
        resources.diagnostics_pending = diagnostics.pending;
        resources
    }

    pub(in crate::runtime) fn protocol_generation(
        &self,
    ) -> Option<crate::generation::RuntimeGeneration> {
        self.lifecycle.access().protocol_generation()
    }

    pub(in crate::runtime) fn last_committed_generation(
        &self,
    ) -> Option<crate::generation::RuntimeGeneration> {
        self.lifecycle.access().last_committed_generation()
    }

    pub(in crate::runtime) fn with_addin_lifecycle<R>(
        &self,
        access: &AddinLifecycleAccess<'_, A>,
        operation: impl FnOnce(&mut A::LifecycleState) -> R,
    ) -> Result<R, xlfn_kernel::thread_affine::ThreadAffineError> {
        self.addin_lifecycle.with_mut(access, operation)
    }

    pub(in crate::runtime) fn take_addin_lifecycle(
        &self,
        access: &AddinLifecycleAccess<'_, A>,
    ) -> Result<TakenLifecycle<'a, A>, LifecycleFailure<'a, A>> {
        match self
            .addin_lifecycle
            .binding(access)
            .map_err(LifecycleFailure::access)?
        {
            BindingState::Occupied(binding) => InstalledLifecycle { binding }.take(),
            BindingState::Empty(_transaction) => {
                Err(LifecycleFailure::access(ThreadAffineError::MissingValue))
            }
        }
    }

    pub(in crate::runtime) fn empty_addin_lifecycle(
        &self,
        access: &AddinLifecycleAccess<'_, A>,
    ) -> Result<EmptyLifecycle<'a, A>, ThreadAffineError> {
        match self.addin_lifecycle.binding(access)? {
            BindingState::Empty(binding) => Ok(EmptyLifecycle { binding }),
            BindingState::Occupied(_) => Err(ThreadAffineError::Occupied),
        }
    }

    pub(in crate::runtime) fn has_addin_lifecycle(
        &self,
        access: &AddinLifecycleAccess<'_, A>,
    ) -> Result<bool, xlfn_kernel::thread_affine::ThreadAffineError> {
        self.addin_lifecycle.has_value(access)
    }

    pub(in crate::runtime) fn release_empty_addin_lifecycle(
        &self,
        access: &AddinLifecycleAccess<'_, A>,
    ) -> Result<(), xlfn_kernel::thread_affine::ThreadAffineError> {
        match self.addin_lifecycle.binding(access)? {
            BindingState::Empty(transaction) => transaction
                .release()
                .map(|_| ())
                .map_err(|error| error.reason),
            BindingState::Occupied(_transaction) => Err(ThreadAffineError::Occupied),
        }
    }

    pub(in crate::runtime) fn close_subscriptions(
        &self,
    ) -> crate::XllResult<crate::shutdown::SubscriptionsStopped> {
        #[cfg(not(feature = "rtd"))]
        {
            Ok(crate::excel_rtd_protocol::stopped_subscriptions(
                self.protocol_generation(),
            ))
        }
        #[cfg(feature = "rtd")]
        {
            let generation = self.protocol_generation();
            let result = self
                .lifecycle
                .with_generation_services(|services| services.close_subscriptions(generation));
            let Some(result) = result else {
                #[cfg(test)]
                {
                    return Ok(crate::excel_rtd_protocol::stopped_subscriptions(generation));
                }
                #[cfg(not(test))]
                {
                    return Err(crate::XllError::Closing);
                }
            };
            result
        }
    }

    pub(in crate::runtime) fn seal_generation_services(
        &self,
        subscriptions_stopped: crate::shutdown::SubscriptionsStopped,
    ) -> crate::XllResult<crate::runtime_components::SealedGenerationServices> {
        crate::runtime_components::SealedGenerationServices::seal_lifecycle(
            self.lifecycle,
            subscriptions_stopped,
        )
    }

    pub(in crate::runtime) fn shutdown_handle_topics(&self) -> crate::XllResult<()> {
        self.lifecycle
            .with_generation_services(GenerationServices::shutdown_handle_topics)
            .unwrap_or(Ok(()))
    }

    pub(in crate::runtime) fn take_generation_for_shutdown(
        &self,
    ) -> Option<crate::generation::ShutdownGeneration<A>> {
        self.lifecycle.take_generation_for_shutdown()
    }

    pub(in crate::runtime) fn quarantine_state(&self) {
        let lifecycle = self.lifecycle_control();
        let mut control = lifecycle.access();
        self.returns.close_admission();
        lifecycle.quarantine_state(&mut control);
    }

    #[cfg(test)]
    pub(in crate::runtime) fn release_test_module_lease(&self) {
        drop(self.lifecycle.test_module_lease.lock().take());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_callback::HostCallbackSession;
    use crate::lifecycle::{HostLifecycleIntent, LifecyclePhase};
    use crate::registration::RegistrationHost;
    use crate::registration::host::test_support::{CallbackScript, Reply};

    #[test]
    fn miri_foreign_runtime_access_is_rejected_without_mutating_issuer_or_source() {
        let issuer = Runtime::<()>::new();
        let foreign = Runtime::<()>::new();
        let access = foreign.bind_addin_lifecycle().unwrap();
        let calls = std::cell::Cell::new(0);

        // Even though a valid capability can operate on its own issuer, a
        // runtime-facing operation must first reject a different issuer.
        assert_eq!(
            issuer.with_addin_lifecycle(&access, |_| calls.set(1)),
            Err(ThreadAffineError::StaleAccess)
        );
        let shutdown = issuer.shutdown_deps();
        assert_eq!(
            shutdown.with_addin_lifecycle(&access, |_| calls.set(2)),
            Err(ThreadAffineError::StaleAccess)
        );
        assert_eq!(
            shutdown.has_addin_lifecycle(&access),
            Err(ThreadAffineError::StaleAccess)
        );
        assert!(matches!(
            shutdown.empty_addin_lifecycle(&access),
            Err(ThreadAffineError::StaleAccess)
        ));
        assert_eq!(
            shutdown.release_empty_addin_lifecycle(&access),
            Err(ThreadAffineError::StaleAccess)
        );
        assert!(shutdown.take_addin_lifecycle(&access).is_err());
        let install = issuer
            .open_deps()
            .install_addin_lifecycle(&access, ())
            .err()
            .expect("foreign issuer");
        assert_eq!(install.reason, ThreadAffineError::StaleAccess);
        assert_eq!(calls.get(), 0);
        assert_eq!(
            foreign.shutdown_deps().has_addin_lifecycle(&access),
            Ok(false)
        );

        let installed = foreign
            .open_deps()
            .install_addin_lifecycle(&access, ())
            .unwrap();
        assert_eq!(
            shutdown.has_addin_lifecycle(&access),
            Err(ThreadAffineError::StaleAccess)
        );
        assert!(shutdown.take_addin_lifecycle(&access).is_err());
        assert_eq!(
            foreign.shutdown_deps().has_addin_lifecycle(&access),
            Ok(true)
        );
        installed
            .take()
            .unwrap()
            .dispose()
            .unwrap()
            .release()
            .unwrap();
    }

    #[test]
    fn rollback_request_rejects_foreign_installed_binding_before_side_effects() {
        let issuer = Runtime::<()>::new();
        let other = Runtime::<()>::new();
        let opening = issuer.begin_open().unwrap();
        let attempt = opening.attempt_id();
        let access = other.bind_addin_lifecycle().unwrap();
        let installed = other
            .open_deps()
            .install_addin_lifecycle(&access, ())
            .unwrap();
        let failure = issuer
            .open_deps()
            .rollback_request(attempt, Some(installed))
            .err()
            .expect("foreign issuer");
        assert_eq!(issuer.phase(), LifecyclePhase::Opening);
        assert_eq!(other.phase(), LifecyclePhase::Closed);
        assert_eq!(other.shutdown_deps().has_addin_lifecycle(&access), Ok(true));
        assert!(
            other.addin_lifecycle.binding(&access).is_err(),
            "failure retains occupied claim"
        );
        drop(failure);
        other
            .shutdown_deps()
            .take_addin_lifecycle(&access)
            .unwrap()
            .dispose()
            .unwrap()
            .release()
            .unwrap();
        drop(opening);
    }

    #[test]
    fn rollback_request_from_a_previous_open_cannot_consume_a_later_attempt() {
        let runtime = Runtime::<()>::new();
        let opening = runtime.begin_open().unwrap();
        let request = runtime
            .open_deps()
            .rollback_request(opening.attempt_id(), None)
            .unwrap();
        let mut opening = runtime.publish(opening, (), ());
        runtime.finish_open(&mut opening, Vec::new()).unwrap();
        let access = runtime.bind_addin_lifecycle().unwrap();
        runtime.remove_addin(&access, &mut HostCallbackSession::new());
        assert_eq!(runtime.phase(), LifecyclePhase::Closed);
        let later = runtime.begin_open().unwrap();
        assert!(!request.matches(runtime.shutdown_deps()));
        let access = runtime.bind_addin_lifecycle().unwrap();
        let outcome = super::super::rollback::rollback_open_request(
            &runtime,
            &access,
            &mut HostCallbackSession::new(),
            request,
        );
        assert!(!outcome.is_finalized());
        assert_eq!(runtime.phase(), LifecyclePhase::Opening);
        assert_eq!(
            runtime.lifecycle.access().open_attempt(),
            Some(later.attempt_id())
        );
        drop(later);
    }

    #[test]
    fn opening_operations_remain_at_the_transaction_issuer() {
        let issuer = Runtime::<()>::new();
        let other = Runtime::<()>::new();
        let transaction = issuer.begin_open().unwrap();
        let generation = transaction.attempt_id().into_runtime_generation();
        let mut transaction = transaction.attach_host(HostCallbackSession::new());
        let deps = transaction.deps();
        let other_control = other.lifecycle_control();
        other_control.request_explicit_removal();

        assert_eq!(deps.host_intent(), HostLifecycleIntent::None);
        assert_eq!(deps.protocol_generation(), Some(generation));
        assert_eq!(other.protocol_generation(), None);
        assert_eq!(
            other.host_intent(),
            HostLifecycleIntent::ExplicitRemovalRequested
        );

        let script = CallbackScript::install([Reply::success(
            xlfn_sys::XL_EVENT_REGISTER,
            xlfn_sys::XLOPER12::integer(7),
        )]);
        let host = RegistrationHost::new(transaction.callbacks_mut());
        deps.ensure_calculation_events(&host, false).unwrap();
        deps.ensure_calculation_events(&host, false).unwrap();
        script.assert_calls(&[xlfn_sys::XL_EVENT_REGISTER]);
        assert_eq!(issuer.resident_event_count(), 1);
        assert_eq!(other.resident_event_count(), 0);
        assert!(issuer.module_residency_held());
        assert!(!other.module_residency_held());

        drop(transaction);
        assert_eq!(issuer.phase(), LifecyclePhase::Quarantined);
        assert_eq!(other.phase(), LifecyclePhase::Closed);
        assert_eq!(
            other.host_intent(),
            HostLifecycleIntent::ExplicitRemovalRequested
        );
        assert_eq!(other.resident_event_count(), 0);
    }
}

#[cfg(test)]
mod binding_tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    struct OnDrop(Box<dyn Fn()>);
    impl Drop for OnDrop {
        fn drop(&mut self) {
            (self.0)();
        }
    }
    struct Probe;
    impl Addin for Probe {
        #[cfg(feature = "rtd")]
        type RtdSubscription = ();
        #[cfg(feature = "rtd")]
        type RtdSources = crate::DynamicRtdSources<()>;
        type SharedState = ();
        type LifecycleState = OnDrop;
        type Layers = ();
        type Error = crate::XllError;
        #[cfg(feature = "async")]
        type AsyncExecutor = crate::NoAsyncExecutor;
        fn open(_: &crate::OpenContext<Self>) -> crate::OpenResult<Self> {
            unreachable!()
        }
    }

    #[test]
    fn lifecycle_disposal_panic_keeps_binding_bound_for_recovery() {
        let slot = ThreadAffineSlot::<OnDrop>::new();
        let access = slot.bind_current().unwrap();
        let BindingState::Empty(empty) = access.binding().unwrap() else {
            panic!("empty")
        };
        let binding = empty
            .install(OnDrop(Box::new(|| panic!("injected lifecycle destructor"))))
            .unwrap_or_else(|_| panic!("install"));
        let taken = InstalledLifecycle::<Probe> { binding }
            .take()
            .unwrap_or_else(|_| panic!("take"));
        let failure = taken.dispose().err().expect("destructor panic");
        assert!(matches!(
            access.binding(),
            Err(ThreadAffineError::ReentrantAccess)
        ));
        drop(failure);
        // Disposition failed, so no successful release proof escaped. Recovery
        // can still observe and handle the same bound, empty slot.
        assert_eq!(access.has_value(), Ok(false));
        let BindingState::Empty(binding) = access.binding().unwrap() else {
            panic!("empty")
        };
        binding.release().unwrap();
    }

    #[cfg(not(miri))]
    #[test]
    #[allow(
        clippy::mem_forget,
        reason = "adversarial forgotten capability must retain payload and occupancy claim"
    )]
    fn forgotten_taken_lifecycle_retains_payload_and_excludes_rebinding() {
        let slot = ThreadAffineSlot::<OnDrop>::new();
        let access = slot.bind_current().unwrap();
        let BindingState::Empty(empty) = access.binding().unwrap() else {
            panic!("empty")
        };
        let destroyed = Rc::new(Cell::new(false));
        let observed = Rc::clone(&destroyed);
        let binding = empty
            .install(OnDrop(Box::new(move || observed.set(true))))
            .unwrap_or_else(|_| panic!("install"));
        let taken = InstalledLifecycle::<Probe> { binding }
            .take()
            .unwrap_or_else(|_| panic!("take"));
        std::mem::forget(taken);
        assert!(!destroyed.get());
        assert!(matches!(
            access.binding(),
            Err(ThreadAffineError::ReentrantAccess)
        ));
    }

    #[test]
    fn lifecycle_disposal_retains_exact_empty_claim_through_reentrant_destructor() {
        let slot = Rc::new(ThreadAffineSlot::<OnDrop>::new());
        let access = slot.bind_current().unwrap();
        let BindingState::Empty(empty) = access.binding().unwrap() else {
            panic!("empty")
        };
        let observed = Rc::new(Cell::new(false));
        let reentrant = Rc::clone(&slot);
        let observed_in_drop = Rc::clone(&observed);
        let value = OnDrop(Box::new(move || {
            let sibling = reentrant.bind_current().unwrap();
            observed_in_drop.set(matches!(
                sibling.binding(),
                Err(ThreadAffineError::ReentrantAccess)
            ));
        }));
        let binding = empty.install(value).unwrap_or_else(|_| panic!("install"));
        let installed = InstalledLifecycle::<Probe> { binding };
        let taken = installed.take().unwrap_or_else(|_| panic!("take"));
        assert!(!observed.get());
        let empty = taken.dispose().unwrap_or_else(|_| panic!("dispose"));
        assert!(observed.get());
        assert!(matches!(
            access.binding(),
            Err(ThreadAffineError::ReentrantAccess)
        ));
        empty.release().unwrap();
        assert!(matches!(
            access.has_value(),
            Err(ThreadAffineError::Unbound)
        ));
    }
}
