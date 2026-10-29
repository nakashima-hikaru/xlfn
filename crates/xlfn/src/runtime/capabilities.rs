//! Operation-scoped views assembled by [`super::Runtime`].
//!
//! These views are deliberately borrowed and private to the runtime domain.
//! They make the facilities used by an operation explicit without turning
//! `Runtime` itself into a service locator available to every transaction.

use super::{AddinLifecycleAccess, Runtime};
use crate::addin::Addin;
use crate::lifecycle::{LifecycleAccess, LifecycleControl, LifecycleCoordinator};
use crate::runtime::observer::RuntimeObserver;
use crate::runtime_components::{
    GenerationServices, HostLedger, ModuleResidency, QuarantineVault, ReturnProtocol,
};
use xlfn_kernel::thread_affine::{ThreadAffineInstallError, ThreadAffineSlot};

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
        generation: Option<crate::generation::RuntimeGeneration>,
    ) -> super::rollback::OpenRollbackOutcome;
    fn quarantine(&self, hazard: Option<crate::shutdown::UnloadHazard>);
}

impl<A: Addin, P: super::UnloadPolicy<A>> OpenRecoveryTarget<A> for Runtime<A, P> {
    fn rollback(
        &self,
        lifecycle: &AddinLifecycleAccess<'_, A>,
        callbacks: &mut crate::host_callback::HostCallbackSession,
        generation: Option<crate::generation::RuntimeGeneration>,
    ) -> super::rollback::OpenRollbackOutcome {
        super::rollback::rollback_open(self, lifecycle, callbacks, generation)
    }
    fn quarantine(&self, hazard: Option<crate::shutdown::UnloadHazard>) {
        match hazard {
            Some(hazard) => super::recovery::quarantine_for_hazard(self, hazard),
            None => super::recovery::quarantine_runtime(self),
        }
    }
}

impl<A: Addin> OpenRecovery<'_, A> {
    pub(in crate::runtime) fn rollback(
        &self,
        lifecycle: &AddinLifecycleAccess<'_, A>,
        callbacks: &mut crate::host_callback::HostCallbackSession,
        generation: Option<crate::generation::RuntimeGeneration>,
    ) -> super::rollback::OpenRollbackOutcome {
        self.target.rollback(lifecycle, callbacks, generation)
    }
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

    pub(in crate::runtime) fn clear_metadata_debt_for_registrations(
        &self,
        registrations: &[crate::registration::RegistrationId],
    ) {
        self.host
            .clear_metadata_debt_for_registrations(registrations);
    }

    pub(in crate::runtime) fn install_addin_lifecycle(
        &self,
        access: &AddinLifecycleAccess<'_, A>,
        state: A::LifecycleState,
    ) -> Result<(), ThreadAffineInstallError<A::LifecycleState>> {
        self.addin_lifecycle.install(access, state)
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
        resources.registrations = self.host.registrations_snapshot().len() as u64;
        resources.resident_event_registrations = self.residency.resident_event_count() as u64;
        resources.module_residency_held = self.residency.is_held();
        resources.registration_state_known = !self.host.registration_state_unknown();
        let callbacks = crate::module_runtime::global().callback_admission();
        resources.callback_gate_open =
            callbacks.lifecycle() == crate::callback_gate::ModuleCallbackLifecycle::Open;
        // The proof has consumed subscription callbacks. Retained host callback
        // reservations must also prevent a falsely clean terminal observation.
        resources.callbacks = callbacks.active() as u64;
        resources.return_blocks = self.returns.returns.outstanding_obligations() as u64;
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
    ) -> Result<A::LifecycleState, xlfn_kernel::thread_affine::ThreadAffineError> {
        self.addin_lifecycle.take(access)
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
        self.addin_lifecycle.release_empty_binding(access)
    }

    pub(in crate::runtime) fn wait_for_return_quiescence(
        &self,
    ) -> crate::XllResult<crate::shutdown::ReturnsQuiescent> {
        crate::shutdown::wait_for_return_quiescence(self.returns)
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
        let generation = self.protocol_generation();
        let mut subscriptions_stopped = Some(subscriptions_stopped);
        let sealed = self.lifecycle.with_generation_services(|services| {
            services.seal(
                generation,
                subscriptions_stopped
                    .take()
                    .expect("generation services consume the subscription certificate once"),
            )
        });
        let Some(sealed) = sealed else {
            return Ok(crate::runtime_components::SealedGenerationServices::empty(
                generation,
                subscriptions_stopped
                    .take()
                    .expect("missing generation services preserve the subscription certificate"),
            ));
        };
        sealed
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
