#[cfg(any(
    test,
    feature = "rtd",
    feature = "bench-internals",
    all(feature = "refinement", any(feature = "handles", feature = "rtd")),
))]
use crate::XllError;
use crate::XllResult;
use crate::addin::PhysicallyUnloadableAddin;
#[cfg(feature = "async")]
use crate::generation::ExecutionLease;
#[cfg(any(test, feature = "refinement"))]
use crate::generation::OpenAttemptId;
#[cfg(any(test, feature = "bench-internals"))]
use crate::generation::OpeningGeneration;
use crate::generation::{ExecutionGeneration, RemovalEpoch, RuntimeGeneration, ShutdownGeneration};
use crate::ingress::AdmittedExport;
#[cfg(any(test, feature = "bench-internals"))]
use crate::registration::RegistrationId;
use std::sync::Arc;
#[cfg(not(feature = "async"))]
use std::sync::atomic::Ordering;

use crate::lifecycle::{
    GenerationAdmission, HostLifecycleIntent, LifecycleCoordinator, LifecyclePhase,
};
#[cfg(any(test, feature = "refinement"))]
use crate::runtime::shutdown::ClosedWitness;
#[cfg(feature = "async")]
use crate::runtime_components::RuntimeExecutors;
use crate::runtime_components::{
    GenerationServices, HostLedger, ModuleResidency, QuarantineReason, QuarantineVault,
    ReturnProtocol, SealedGenerationServices,
};
#[cfg(any(test, feature = "bench-internals"))]
use crate::runtime_open_txn::OpeningTxn;
use crate::runtime_refinement::RuntimeRefinementHooks;
use xlfn_kernel::thread_affine::{
    ThreadAffineAccess, ThreadAffineError, ThreadAffineInstallError, ThreadAffineSlot,
};

pub(crate) mod shutdown;

type QuiesceOperation<A> = fn(
    &mut <A as crate::Addin>::SharedState,
    &mut <A as crate::Addin>::LifecycleState,
) -> Result<(), <A as crate::Addin>::Error>;

fn logical_quiesce<A: crate::Addin>(
    shared: &mut A::SharedState,
    lifecycle: &mut A::LifecycleState,
) -> Result<(), A::Error> {
    A::quiesce(shared, lifecycle)
}

fn physical_quiesce<A: PhysicallyUnloadableAddin>(
    shared: &mut A::SharedState,
    lifecycle: &mut A::LifecycleState,
) -> Result<(), A::Error> {
    A::quiesce_for_physical_unload(shared, lifecycle)
}

/// Couples the quiesce operation with the residency policy that selected it.
/// A runtime can therefore not contain a physical-unload flag that disagrees
/// with the callback used to quiesce the add-in.
enum UnloadPolicy<A: crate::Addin> {
    Logical,
    Physical(QuiesceOperation<A>),
}

impl<A: crate::Addin> UnloadPolicy<A> {
    fn quiesce(
        &self,
        shared: &mut A::SharedState,
        lifecycle: &mut A::LifecycleState,
    ) -> Result<(), A::Error> {
        match self {
            Self::Logical => logical_quiesce::<A>(shared, lifecycle),
            Self::Physical(operation) => operation(shared, lifecycle),
        }
    }

    const fn is_physical(&self) -> bool {
        matches!(self, Self::Physical(_))
    }
}

pub struct Runtime<A: crate::Addin> {
    pub(crate) lifecycle: LifecycleCoordinator<A>,
    pub(crate) addin_lifecycle: ThreadAffineSlot<A::LifecycleState>,
    pub(crate) host: HostLedger,
    pub(crate) return_protocol: ReturnProtocol,
    #[cfg(feature = "async")]
    pub(crate) executors: RuntimeExecutors,
    pub(crate) residency: ModuleResidency,
    unload_policy: UnloadPolicy<A>,
    pub(crate) quarantine: QuarantineVault<A>,
    pub(crate) refinement: RuntimeRefinementHooks,
}

pub(crate) type AddinLifecycleAccess<'runtime, A> =
    ThreadAffineAccess<'runtime, <A as crate::Addin>::LifecycleState>;

impl<A: crate::Addin> Runtime<A> {
    pub(crate) fn lifecycle_control(&self) -> crate::lifecycle::LifecycleControl<'_, A> {
        crate::lifecycle::LifecycleControl::new(&self.lifecycle)
    }

    pub(crate) fn runtime_orchestrator(
        &self,
    ) -> crate::runtime_orchestration::RuntimeOrchestrator<'_, A> {
        crate::runtime_orchestration::RuntimeOrchestrator::new(self)
    }
}

impl<A: crate::Addin> Runtime<A> {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            lifecycle: LifecycleCoordinator::new(),
            addin_lifecycle: ThreadAffineSlot::new(),
            host: HostLedger::new(),
            return_protocol: ReturnProtocol::new(),
            #[cfg(feature = "async")]
            executors: RuntimeExecutors::new(),
            residency: ModuleResidency::new(),
            unload_policy: UnloadPolicy::Logical,
            quarantine: QuarantineVault::new(),
            refinement: RuntimeRefinementHooks::new(),
        }
    }

    /// Constructs a runtime whose terminal close may release the DLL's
    /// physical residency lease. The unsafe contract is carried by the
    /// `PhysicallyUnloadableAddin` implementation.
    #[must_use]
    pub const fn new_with_physical_unload() -> Self
    where
        A: PhysicallyUnloadableAddin,
    {
        Self {
            lifecycle: LifecycleCoordinator::new(),
            addin_lifecycle: ThreadAffineSlot::new(),
            host: HostLedger::new(),
            return_protocol: ReturnProtocol::new(),
            #[cfg(feature = "async")]
            executors: RuntimeExecutors::new(),
            residency: ModuleResidency::new(),
            unload_policy: UnloadPolicy::Physical(physical_quiesce::<A>),
            quarantine: QuarantineVault::new(),
            refinement: RuntimeRefinementHooks::new(),
        }
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn refinement_hooks(&self) -> &RuntimeRefinementHooks {
        &self.refinement
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn composition_trace(&self) -> &crate::composition_refinement::CompositionTrace {
        self.refinement.composition_trace()
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn record_composition_event(
        &self,
        event: crate::composition_refinement::CompositionEvent,
    ) {
        self.composition_trace().record(event);
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn record_composition_begin_open(&self, sampled_epoch: u64, attempt: u64) {
        self.composition_trace().begin_open(sampled_epoch, attempt);
    }

    #[cfg(any(test, feature = "refinement"))]
    fn mark_composition_return_pending(&self) {
        self.composition_trace().mark_return_pending();
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn finish_composition_return(&self) {
        self.composition_trace().finish_return();
    }

    // This is called by the explicit removal boundary after the terminal
    // teardown has returned AlreadyClosed; begin_final_removal only records its
    // lifecycle request and does not claim the host call returned successfully.
    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn record_composition_already_closed_return(&self) {
        self.mark_composition_return_pending();
        self.finish_composition_return();
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn mark_composition_terminal_pending(&self) {
        self.composition_trace().mark_terminal_pending();
    }

    #[must_use]
    pub fn phase(&self) -> LifecyclePhase {
        self.lifecycle.observed_phase()
    }

    pub(crate) fn host_intent(&self) -> HostLifecycleIntent {
        self.lifecycle.access().host_intent()
    }

    /// Acquires the DLL's self-reference before a generated `xlAutoOpen`
    /// enters the logical opening transaction.
    pub(crate) fn ensure_module_residency(&self, anchor: *const ()) -> XllResult<bool> {
        self.residency.ensure(anchor)
    }

    /// Releases the physical residency reference after explicit removal has
    /// completed. Ordinary host shutdown hints never call this method.
    pub(crate) fn release_module_residency(&self) -> XllResult<()> {
        self.residency.release()
    }

    pub(crate) fn physical_unload_enabled(&self) -> bool {
        self.unload_policy.is_physical()
    }

    pub(crate) fn quiesce_addin(
        &self,
        shared: &mut A::SharedState,
        lifecycle: &mut A::LifecycleState,
    ) -> XllResult<()> {
        self.unload_policy
            .quiesce(shared, lifecycle)
            .map_err(crate::error::IntoXllError::into_xll_error)
    }

    #[cfg(any(feature = "rtd", feature = "handles", test))]
    pub(crate) fn module_residency_held(&self) -> bool {
        self.residency.is_held()
    }

    pub(crate) fn quarantine_snapshot(&self) -> Vec<(Option<RuntimeGeneration>, QuarantineReason)> {
        self.quarantine.snapshot()
    }

    pub(crate) fn last_committed_generation(&self) -> Option<RuntimeGeneration> {
        self.lifecycle.access().last_committed_generation()
    }

    pub(crate) fn protocol_generation(&self) -> Option<RuntimeGeneration> {
        self.lifecycle.access().protocol_generation()
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn open_attempt(&self) -> Option<OpenAttemptId> {
        self.lifecycle.access().open_attempt()
    }

    pub(crate) fn removal_epoch(&self) -> RemovalEpoch {
        RemovalEpoch::new(self.lifecycle.access().removal_epoch())
    }

    #[cfg(any(test, feature = "bench-internals"))]
    pub(crate) fn publish(&self, state: A::SharedState, layers: A::Layers)
    where
        A::LifecycleState: Default,
    {
        self.publish_with_lifecycle(state, Default::default(), layers);
    }

    #[cfg(any(test, feature = "bench-internals"))]
    pub(crate) fn publish_with_lifecycle(
        &self,
        state: A::SharedState,
        lifecycle_state: A::LifecycleState,
        layers: A::Layers,
    ) {
        let access = self
            .bind_addin_lifecycle()
            .expect("test runtime binds its lifecycle thread");
        if self.with_addin_lifecycle(&access, |_| ()).is_err() {
            assert!(
                self.install_addin_lifecycle(&access, lifecycle_state)
                    .is_ok(),
                "test runtime has one lifecycle state"
            );
        }
        let attempt = self
            .lifecycle
            .access()
            .open_attempt()
            .expect("test publish requires an open attempt");
        assert!(
            self.lifecycle_control()
                .stage_opening_generation(
                    attempt,
                    OpeningGeneration {
                        shared_state: state,
                        layers,
                        init_config: crate::addin::RuntimeConfig::new(),
                    },
                )
                .is_ok()
        );
    }

    pub(crate) fn bind_addin_lifecycle(
        &self,
    ) -> Result<AddinLifecycleAccess<'_, A>, ThreadAffineError> {
        self.addin_lifecycle.bind_current()
    }

    pub(crate) fn install_addin_lifecycle(
        &self,
        access: &AddinLifecycleAccess<'_, A>,
        state: A::LifecycleState,
    ) -> Result<(), ThreadAffineInstallError<A::LifecycleState>> {
        self.addin_lifecycle.install(access, state)
    }

    pub(crate) fn with_addin_lifecycle<R>(
        &self,
        access: &AddinLifecycleAccess<'_, A>,
        operation: impl FnOnce(&mut A::LifecycleState) -> R,
    ) -> Result<R, ThreadAffineError> {
        self.addin_lifecycle.with_mut(access, operation)
    }

    pub(crate) fn has_addin_lifecycle(
        &self,
        access: &AddinLifecycleAccess<'_, A>,
    ) -> Result<bool, ThreadAffineError> {
        self.addin_lifecycle.has_value(access)
    }

    pub(crate) fn take_addin_lifecycle(
        &self,
        access: &AddinLifecycleAccess<'_, A>,
    ) -> Result<A::LifecycleState, ThreadAffineError> {
        self.addin_lifecycle.take(access)
    }

    pub(crate) fn release_empty_addin_lifecycle(
        &self,
        access: &AddinLifecycleAccess<'_, A>,
    ) -> Result<(), ThreadAffineError> {
        self.addin_lifecycle.release_empty_binding(access)
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn has_opening_generation(&self) -> bool {
        self.lifecycle.has_opening_generation()
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn has_current_generation(&self) -> bool {
        self.lifecycle.has_current_generation()
    }

    #[cfg(any(all(test, feature = "handles"), feature = "bench-internals"))]
    pub(crate) fn arm_test_generation(&self) {
        let services = GenerationServices::arm_generation(
            crate::generation::RuntimeGeneration::new(1).expect("test generation is non-zero"),
            crate::addin::RuntimeConfig::new(),
            Some(crate::excel_rtd::RtdSubscriptionHost::detached()),
        )
        .expect("test runtime generation can be armed once")
        .commit();
        self.lifecycle.install_test_generation_services(services);
    }

    #[cfg(test)]
    pub(crate) fn take_current_generation(&self) -> Option<Arc<ExecutionGeneration<A>>> {
        self.lifecycle.take_current_generation()
    }

    pub(crate) fn take_generation_for_shutdown(&self) -> Option<ShutdownGeneration<A>> {
        self.lifecycle.take_generation_for_shutdown()
    }

    #[cfg(any(test, feature = "bench-internals"))]
    pub(crate) fn finish_open<Stage>(
        &self,
        attempt: &mut OpeningTxn<'_, A, Stage>,
        registrations: Vec<RegistrationId>,
    ) -> XllResult<()> {
        attempt.commit_in_place(registrations)
    }

    pub(crate) fn enter<'call>(
        &'call self,
        ingress: &'call AdmittedExport<'call>,
    ) -> XllResult<CallGuard<'call, A>> {
        ingress.assert_active();
        let admission = self.lifecycle.try_admit()?;
        #[cfg(any(test, feature = "refinement"))]
        let activity_id = self.refinement_hooks().next_activity_id();
        #[cfg(any(test, feature = "refinement"))]
        self.refinement_hooks().call_entered(self, activity_id);
        Ok(CallGuard {
            #[cfg(any(test, feature = "refinement"))]
            runtime: self,
            #[cfg(not(any(test, feature = "refinement")))]
            _runtime: std::marker::PhantomData,
            admission,
            _ingress: ingress,
            #[cfg(any(test, feature = "refinement"))]
            activity_id,
        })
    }

    #[inline]
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "Method used in test suite and internal diagnostics"
        )
    )]
    pub(crate) const fn return_tracker(&self) -> &crate::return_value::ReturnTracker {
        &self.return_protocol.returns
    }

    #[inline]
    pub(crate) fn enter_return_producer(
        &'static self,
    ) -> Option<crate::return_value::ReturnProducerGuard<'static>> {
        self.return_protocol.enter_producer()
    }

    #[inline]
    #[cfg(test)]
    pub(crate) fn wait_for_returns(&self) {
        self.return_protocol.wait_for_returns();
    }

    pub(crate) fn wait_for_return_quiescence(
        &self,
    ) -> XllResult<crate::shutdown::ReturnsQuiescent> {
        crate::shutdown::wait_for_return_quiescence(&self.return_protocol)
    }

    #[inline]
    pub(crate) fn returns_are_quiescent(&self) -> bool {
        self.return_protocol.returns_are_quiescent()
    }

    #[cfg(test)]
    pub(crate) fn disable_trace_for_test(&self) {
        self.refinement_hooks().disable_for_test();
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn record_returned_success(&self, witness: ClosedWitness) -> XllResult<()> {
        self.refinement_hooks()
            .record_returned_success(self, &witness)?;
        self.mark_composition_return_pending();
        Ok(())
    }

    #[cfg(all(test, feature = "rtd", feature = "handles"))]
    pub(crate) fn shutdown_trace_json(&self) -> String {
        self.refinement_hooks()
            .trace_handle()
            .trace_json()
            .expect("shutdown trace is not active")
    }

    #[cfg(test)]
    pub(crate) fn composition_trace_json(&self) -> String {
        self.composition_trace()
            .trace_json()
            .expect("composition trace serialization")
    }
}

impl<A: crate::Addin> Runtime<A> {
    pub(crate) fn next_call_id(&self) -> u64 {
        self.return_protocol.next_call_id()
    }

    #[cfg(test)]
    pub(crate) fn peek_next_call_id(&self) -> u64 {
        self.return_protocol.peek_next_call_id()
    }

    pub(crate) fn calculation_id(&self) -> crate::execution::CalculationId {
        #[cfg(feature = "async")]
        {
            crate::execution::CalculationId::new(self.executors.async_manager.current_generation())
        }
        #[cfg(not(feature = "async"))]
        {
            crate::execution::CalculationId::new(
                self.return_protocol.calculation_id.load(Ordering::Acquire),
            )
        }
    }

    #[cfg(feature = "async")]
    pub(crate) fn finish_calculation(&self) {
        let _ = self.executors.async_manager.advance_generation();
    }

    #[cfg(all(feature = "handles", any(test, feature = "bench-internals")))]
    pub(crate) fn formula_handle_service(
        &self,
    ) -> XllResult<Arc<crate::handle::FormulaHandleService>> {
        self.generation_services()?.formula_handle_service()
    }

    fn generation_services_snapshot(&self) -> Option<Arc<GenerationServices>> {
        self.lifecycle.load_generation_services().or_else(|| {
            let control = self.lifecycle.access();
            control.retiring_services().map(Arc::clone)
        })
    }

    #[cfg(any(
        feature = "bench-internals",
        all(feature = "refinement", any(feature = "handles", feature = "rtd")),
        all(test, any(feature = "handles", feature = "rtd")),
    ))]
    pub(crate) fn generation_services(&self) -> XllResult<Arc<GenerationServices>> {
        let services = self.generation_services_snapshot();
        services.ok_or(XllError::Closing)
    }

    pub(crate) fn seal_generation_services(
        &self,
        subscriptions_stopped: crate::shutdown::SubscriptionsStopped,
    ) -> XllResult<SealedGenerationServices> {
        let generation = self.protocol_generation();
        let Some(services) = self.generation_services_snapshot() else {
            return Ok(SealedGenerationServices::empty(
                generation,
                subscriptions_stopped,
            ));
        };
        services.seal(generation, subscriptions_stopped)
    }

    pub(crate) fn shutdown_handle_topics(&self) -> XllResult<()> {
        let Some(services) = self.generation_services_snapshot() else {
            return Ok(());
        };
        services.shutdown_handle_topics()
    }

    #[cfg(test)]
    pub(crate) fn finish_generation_services(
        &self,
        sealed: SealedGenerationServices,
    ) -> XllResult<(
        crate::shutdown::HandlesQuiescent,
        crate::shutdown::SubscriptionsStopped,
    )> {
        sealed.finish()
    }

    #[inline]
    #[cfg(all(test, feature = "rtd"))]
    pub(crate) fn subscriptions(&self) -> XllResult<crate::excel_rtd::SubscriptionRuntimeRead> {
        let services = self.generation_services()?;
        services.rtd_call_access().read()
    }

    pub(crate) fn close_subscriptions(&self) -> XllResult<crate::shutdown::SubscriptionsStopped> {
        #[cfg(not(feature = "rtd"))]
        {
            Ok(crate::excel_rtd::stopped_subscriptions(
                self.protocol_generation(),
            ))
        }
        #[cfg(feature = "rtd")]
        {
            let Some(services) = self.generation_services_snapshot() else {
                #[cfg(test)]
                {
                    return Ok(crate::excel_rtd::stopped_subscriptions(
                        self.protocol_generation(),
                    ));
                }
                #[cfg(not(test))]
                {
                    return Err(XllError::Closing);
                }
            };
            services.close_subscriptions(self.protocol_generation())
        }
    }

    #[cfg(feature = "async")]
    pub(crate) fn start_async(&self, worker_count: usize) -> XllResult<()> {
        self.executors.async_manager.start(worker_count)
    }

    #[cfg(feature = "async")]
    pub(crate) fn cancel_async(&self) {
        self.executors.async_manager.cancel_current_generation();
    }

    #[cfg(feature = "async")]
    pub(crate) fn close_async(
        &self,
    ) -> crate::shutdown::StopOutcome<crate::shutdown::AsyncStopped> {
        self.executors.async_manager.close()
    }

    #[cfg(feature = "async")]
    pub(crate) fn async_manager(&self) -> &crate::async_udf::AsyncManager {
        &self.executors.async_manager
    }

    #[cfg(test)]
    pub(crate) fn release_test_module_lease(&self) {
        drop(self.lifecycle.test_module_lease.lock().take());
    }
}

impl<A: crate::Addin> Default for Runtime<A> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
impl<A: crate::Addin> Runtime<A> {
    pub(crate) fn cleanup_test_runtime(&self) {
        if !matches!(self.phase(), LifecyclePhase::Closed) {
            let ingress = crate::module_runtime::ingress();
            if matches!(
                ingress.phase(),
                crate::ingress::PHASE_OPENING | crate::ingress::PHASE_OPEN
            ) {
                ingress.begin_close_with(|| {});
            }
            if ingress.phase() == crate::ingress::PHASE_CLOSING {
                let _ = ingress.seal_and_drain();
            }
        }
        drop(self.lifecycle.test_module_lease.lock().take());
    }
}

#[cfg(test)]
impl<A: crate::Addin> Drop for Runtime<A> {
    fn drop(&mut self) {
        self.cleanup_test_runtime();
    }
}

#[cfg(test)]
pub(crate) struct StaticTestRuntime<A: crate::Addin> {
    runtime: &'static Runtime<A>,
}

#[cfg(test)]
impl<A: crate::Addin> StaticTestRuntime<A> {
    pub(crate) fn new() -> Self {
        let runtime = Box::leak(Box::new(Runtime::new()));
        Self { runtime }
    }

    pub(crate) fn runtime(&self) -> &'static Runtime<A> {
        self.runtime
    }
}

#[cfg(test)]
impl<A: crate::Addin> Drop for StaticTestRuntime<A> {
    fn drop(&mut self) {
        self.runtime.cleanup_test_runtime();
    }
}

pub struct CallGuard<'runtime, A: crate::Addin> {
    _ingress: &'runtime AdmittedExport<'runtime>,
    #[cfg(any(test, feature = "refinement"))]
    runtime: &'runtime Runtime<A>,
    #[cfg(not(any(test, feature = "refinement")))]
    _runtime: std::marker::PhantomData<&'runtime Runtime<A>>,
    admission: GenerationAdmission<A>,
    #[cfg(any(test, feature = "refinement"))]
    activity_id: crate::shutdown_trace::ActivityId,
}

impl<A: crate::Addin> CallGuard<'_, A> {
    #[must_use]
    pub fn state(&self) -> &A::SharedState {
        &self.generation().shared_state
    }

    #[must_use]
    pub(crate) fn layers(&self) -> &A::Layers {
        &self.generation().layers
    }

    #[cfg(feature = "handles")]
    #[must_use]
    pub(crate) fn handle_call_access(&self) -> crate::handle::FormulaHandleServiceResolver<'_> {
        self.admission.services().handle_call_access()
    }

    #[cfg(feature = "rtd")]
    #[must_use]
    pub(crate) fn rtd_call_access(&self) -> crate::rtd::RtdGenerationAccess<'_> {
        self.admission.services().rtd_call_access()
    }

    fn generation(&self) -> &ExecutionGeneration<A> {
        let generation = self.admission.generation();
        let _ = generation.id();
        generation
    }

    #[cfg(feature = "async")]
    #[must_use]
    pub(crate) fn lease(&self) -> ExecutionLease<A> {
        ExecutionLease {
            generation: Arc::clone(self.admission.generation_arc()),
        }
    }
}

impl<A: crate::Addin> Drop for CallGuard<'_, A> {
    fn drop(&mut self) {
        #[cfg(any(test, feature = "refinement"))]
        self.runtime
            .refinement_hooks()
            .call_left(self.runtime, self.activity_id);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::runtime::shutdown::{FinalRemoval, OpenRollback, QuiescenceProof, RemovalOwner};
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    #[derive(Clone)]
    struct TestU32Addin;

    impl crate::Addin for TestU32Addin {
        type SharedState = u32;
        type LifecycleState = ();
        type Error = XllError;
        type Layers = ();

        fn open(
            _context: &crate::addin::OpenContext,
        ) -> Result<
            crate::addin::Opened<Self::SharedState, Self::LifecycleState, Self::Layers>,
            Self::Error,
        > {
            Ok(crate::addin::Opened::new(0, (), ()))
        }
    }

    fn admitted_export() -> crate::ingress::AdmittedExport<'static> {
        crate::module_runtime::ingress()
            .enter_with(|| {})
            .into_admitted()
            .expect("test call enters during OPEN")
    }

    fn finish_test_close<A: crate::Addin>(
        runtime: &Runtime<A>,
        mut removal_attempt: RemovalOwner<'_, A>,
    ) {
        let module_closing = removal_attempt.take_module_closing();
        let drained = module_closing.seal_and_drain();
        let (module_quiescent, _exports) = drained.certify();
        let module_epoch = module_quiescent.id();
        let subscriptions_stopped = runtime
            .close_subscriptions()
            .expect("test subscriptions stop");
        let _ = runtime.shutdown_handle_topics();
        let sealed = runtime
            .seal_generation_services(subscriptions_stopped)
            .expect("test generation service seal");
        let _ = runtime.finish_generation_services(sealed);
        // This helper validates Runtime's close certificate in isolation. It
        // deliberately does not synthesize lifecycle trace milestones; those
        // are exercised by the real lifecycle close path.
        runtime.disable_trace_for_test();
        let _rtd = crate::excel_rtd::wait_for_module_quiescence().expect("RTD module quiescence");
        let last_generation = runtime.lifecycle.access().last_committed_generation();
        let certificate = removal_attempt
            .certify::<FinalRemoval>(QuiescenceProof::for_test(last_generation, module_epoch))
            .map_err(|(error, _owner)| error)
            .unwrap();
        let (_witness, _removal_attempt) = certificate
            .finish()
            .unwrap_or_else(|(error, _certificate)| panic!("{error}"));
        runtime.release_test_module_lease();
    }

    fn finish_test_open_rollback<'a, A: crate::Addin>(
        runtime: &'a Runtime<A>,
        mut rollback_attempt: RemovalOwner<'a, A>,
    ) -> RemovalOwner<'a, A> {
        let module_closing = rollback_attempt.take_module_closing();
        let drained = module_closing.seal_and_drain();
        let (module_quiescent, _exports) = drained.certify();
        let module_epoch = module_quiescent.id();
        let certificate = rollback_attempt
            .certify::<OpenRollback>(QuiescenceProof::for_test(
                Some(crate::generation::RuntimeGeneration::new(1).unwrap()),
                module_epoch,
            ))
            .map_err(|(error, _owner)| error)
            .unwrap();
        let rollback_attempt = certificate
            .finish()
            .unwrap_or_else(|(error, _certificate)| panic!("{error}"));
        runtime.release_test_module_lease();
        rollback_attempt
    }

    pub(crate) static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[cfg(feature = "handles")]
    #[test]
    fn runtime_can_open_close_and_reopen() {
        let _test_guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        struct TestHandle(u32);
        impl crate::handle::ExcelHandleObject for TestHandle {}

        let runtime = Runtime::<TestU32Addin>::new();
        let mut open_attempt = runtime.runtime_orchestrator().begin_open().unwrap();
        runtime.publish(1_u32, ());
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
        let ingress = admitted_export();
        assert_eq!(runtime.enter(&ingress).unwrap().state(), &1);
        let old_handles = runtime.formula_handle_service().unwrap();
        let old_token = old_handles
            .prepare(crate::handle::test_topic_key("old"), || Ok(TestHandle(1)))
            .unwrap()
            .into_token();
        drop(ingress);

        let removal_attempt = runtime
            .runtime_orchestrator()
            .begin_final_removal()
            .unwrap();
        assert_eq!(runtime.take_current_generation().unwrap().shared_state, 1);
        finish_test_close(&runtime, removal_attempt);

        let mut open_attempt = runtime.runtime_orchestrator().begin_open().unwrap();
        runtime.publish(2_u32, ());
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
        let ingress = admitted_export();
        assert_eq!(runtime.enter(&ingress).unwrap().state(), &2);
        let new_handles = runtime.formula_handle_service().unwrap();
        let new_token = new_handles
            .prepare(crate::handle::test_topic_key("new"), || Ok(TestHandle(2)))
            .unwrap()
            .into_token();
        assert_eq!(
            crate::value::with_excel_call_scope(|scope| {
                new_handles
                    .lookup::<TestHandle>(scope, &new_token)
                    .map(|value| value.0)
            })
            .unwrap(),
            2
        );
        assert!(matches!(
            crate::value::with_excel_call_scope(|scope| {
                new_handles
                    .lookup::<TestHandle>(scope, &old_token)
                    .map(|_| ())
            }),
            Err(XllError::StaleHandle | XllError::InvalidHandle)
        ));
        drop(ingress);

        let removal_attempt = runtime
            .runtime_orchestrator()
            .begin_final_removal()
            .unwrap();
        assert_eq!(runtime.take_current_generation().unwrap().shared_state, 2);
        finish_test_close(&runtime, removal_attempt);
    }

    #[test]
    fn close_on_closed_runtime_invalidates_an_older_open_epoch() {
        let _test_guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let runtime = Runtime::<()>::new();
        let stale_epoch = runtime.removal_epoch();

        assert!(
            runtime
                .runtime_orchestrator()
                .begin_final_removal()
                .is_none()
        );
        assert!(
            runtime
                .runtime_orchestrator()
                .begin_open_if_epoch(stale_epoch)
                .is_err()
        );

        let mut current = runtime.runtime_orchestrator().begin_open().unwrap();
        runtime.publish((), ());
        runtime.finish_open(&mut current, Vec::new()).unwrap();
        assert_eq!(runtime.phase(), LifecyclePhase::Open);
    }

    #[test]
    fn a_failed_concurrent_open_cannot_rollback_the_active_attempt() {
        let _test_guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let runtime = Runtime::<TestU32Addin>::new();
        let mut first = runtime.runtime_orchestrator().begin_open().unwrap();

        assert!(runtime.runtime_orchestrator().begin_open().is_err());
        assert_eq!(runtime.phase(), LifecyclePhase::Opening);

        runtime.publish(11_u32, ());
        runtime.finish_open(&mut first, Vec::new()).unwrap();
        assert_eq!(runtime.phase(), LifecyclePhase::Open);
        let ingress = admitted_export();
        assert_eq!(runtime.enter(&ingress).unwrap().state(), &11);
        drop(ingress);
        let close = runtime
            .runtime_orchestrator()
            .begin_final_removal()
            .unwrap();
        let _ = runtime.take_current_generation();
        finish_test_close(&runtime, close);
    }

    #[test]
    fn dropping_open_attempt_quarantines_without_implicit_rollback() {
        let _test_guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let runtime = Runtime::<()>::new();
        let opening = runtime.runtime_orchestrator().begin_open().unwrap();

        drop(opening);

        assert_eq!(runtime.phase(), LifecyclePhase::Quarantined);
        let trace = runtime.composition_trace_json();
        assert!(!trace.contains("\"failOpen\""));
        assert!(
            runtime
                .runtime_orchestrator()
                .acquire_open_rollback()
                .is_none()
        );
    }

    #[test]
    fn final_close_cancels_an_in_flight_open_commit() {
        let _test_guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let runtime = Arc::new(Runtime::<TestU32Addin>::new());
        let mut opening = runtime.runtime_orchestrator().begin_open().unwrap();
        runtime.publish(17_u32, ());

        let removal_epoch = runtime.removal_epoch();
        let closing_runtime = Arc::clone(&runtime);
        let (closing_entered_tx, closing_entered_rx) = mpsc::channel();
        let (closing_release_tx, closing_release_rx) = mpsc::channel();
        let (closed_tx, closed_rx) = mpsc::sync_channel(1);
        let closer = thread::spawn(move || {
            let close = closing_runtime
                .runtime_orchestrator()
                .begin_final_removal()
                .expect("the opening runtime requires final close");
            closing_entered_tx.send(()).unwrap();
            closing_release_rx.recv().unwrap();
            let state = match closing_runtime
                .take_generation_for_shutdown()
                .expect("shutdown extracts generation")
            {
                ShutdownGeneration::Open(generation) => generation.shared_state,
                ShutdownGeneration::Opening(opening) => opening.into_parts().0,
            };
            assert_eq!(state, 17);
            finish_test_close(&closing_runtime, close);
            closed_tx.send(()).unwrap();
        });

        let deadline = Instant::now() + Duration::from_secs(5);
        while runtime.phase() != LifecyclePhase::Closing && Instant::now() < deadline {
            thread::yield_now();
        }
        assert_eq!(runtime.phase(), LifecyclePhase::Closing);
        assert_ne!(runtime.removal_epoch(), removal_epoch);
        assert!(matches!(
            runtime.finish_open(&mut opening, Vec::new()),
            Err(XllError::Closing)
        ));
        assert_eq!(runtime.open_attempt(), None);

        closing_entered_rx.recv().unwrap();
        closing_release_tx.send(()).unwrap();

        closed_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        closer.join().unwrap();
        assert_eq!(runtime.phase(), LifecyclePhase::Closed);
    }

    #[test]
    fn logical_quiescence_certificate_survives_a_concurrent_removal_epoch_bump() {
        let _test_guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let runtime = Arc::new(Runtime::<()>::new());
        let mut opening = runtime.runtime_orchestrator().begin_open().unwrap();
        runtime.publish((), ());
        runtime.finish_open(&mut opening, Vec::new()).unwrap();

        let mut removal_attempt = runtime
            .runtime_orchestrator()
            .begin_final_removal()
            .unwrap();
        runtime.wait_for_returns();
        let subscriptions_stopped = runtime.close_subscriptions().unwrap();
        runtime.shutdown_handle_topics().unwrap();
        let sealed = runtime
            .seal_generation_services(subscriptions_stopped)
            .unwrap();
        runtime.finish_generation_services(sealed).unwrap();
        assert!(runtime.take_current_generation().is_some());

        let module_closing = removal_attempt.take_module_closing();
        let drained = module_closing.seal_and_drain();
        let (module_quiescent, _exports) = drained.certify();
        let module_epoch = module_quiescent.id();
        runtime.disable_trace_for_test();
        let _rtd = crate::excel_rtd::wait_for_module_quiescence().expect("RTD module quiescence");
        let certificate = removal_attempt
            .certify::<FinalRemoval>(QuiescenceProof::for_test(
                Some(crate::generation::RuntimeGeneration::new(1).unwrap()),
                module_epoch,
            ))
            .map_err(|(error, _owner)| error)
            .unwrap();

        // A second final-close invocation invalidates stale open attempts, but
        // it must not invalidate the certificate held by the active close
        // owner. The second caller waits until that owner is released.
        let removal_epoch = runtime.removal_epoch();
        let concurrent_runtime = Arc::clone(&runtime);
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let waiter = thread::spawn(move || {
            started_tx.send(()).unwrap();
            assert!(
                concurrent_runtime
                    .runtime_orchestrator()
                    .begin_final_removal()
                    .is_none()
            );
        });
        started_rx.recv().unwrap();

        let deadline = Instant::now() + Duration::from_secs(1);
        while runtime.removal_epoch() == removal_epoch && Instant::now() < deadline {
            thread::yield_now();
        }
        assert_ne!(runtime.removal_epoch(), removal_epoch);

        let (_witness, removal_attempt) = certificate
            .finish()
            .unwrap_or_else(|(error, _certificate)| panic!("{error}"));
        drop(removal_attempt);
        waiter.join().unwrap();
        assert_eq!(runtime.phase(), LifecyclePhase::Closed);
    }

    #[test]
    fn close_waiter_is_not_lost_when_open_rollback_finishes() {
        let _test_guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let runtime = Arc::new(Runtime::<()>::new());
        let mut opening = runtime.runtime_orchestrator().begin_open().unwrap();
        assert!(opening.fail().requires_rollback());
        let rollback = runtime
            .runtime_orchestrator()
            .acquire_open_rollback()
            .unwrap();

        let closing_runtime = Arc::clone(&runtime);
        let (closed_tx, closed_rx) = mpsc::sync_channel(1);
        let closer = thread::spawn(move || {
            assert!(
                closing_runtime
                    .runtime_orchestrator()
                    .begin_final_removal()
                    .is_none()
            );
            closed_tx.send(()).unwrap();
        });

        let deadline = Instant::now() + Duration::from_secs(1);
        while runtime.phase() != LifecyclePhase::Closing && Instant::now() < deadline {
            thread::yield_now();
        }
        assert_eq!(runtime.phase(), LifecyclePhase::Closing);
        let rollback = finish_test_open_rollback(&runtime, rollback);
        assert!(closed_rx.recv_timeout(Duration::from_millis(20)).is_err());
        drop(rollback);

        closed_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        closer.join().unwrap();
        assert_eq!(runtime.phase(), LifecyclePhase::Closed);
    }

    #[test]
    fn abandoned_close_owner_notifies_and_allows_takeover() {
        let _test_guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let runtime = Arc::new(Runtime::<()>::new());
        let mut opening = runtime.runtime_orchestrator().begin_open().unwrap();
        runtime.publish((), ());
        runtime.finish_open(&mut opening, Vec::new()).unwrap();

        let first = runtime
            .runtime_orchestrator()
            .begin_final_removal()
            .unwrap();
        drop(first);

        let second = runtime
            .runtime_orchestrator()
            .begin_final_removal()
            .unwrap();
        let _ = runtime.take_current_generation();
        finish_test_close(&runtime, second);
        assert_eq!(runtime.phase(), LifecyclePhase::Closed);
    }

    #[test]
    fn lifecycle_attempt_counter_refuses_exhaustion() {
        let _test_guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let runtime = Runtime::<()>::new();
        runtime
            .lifecycle
            .access()
            .set_next_lifecycle_attempt_for_test(u64::MAX);
        assert!(runtime.runtime_orchestrator().begin_open().is_err());
        assert_eq!(runtime.phase(), LifecyclePhase::Closed);
    }

    #[test]
    fn logical_quiescence_certificate_refuses_to_publish_closed_before_state_is_released() {
        let _test_guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let runtime = Runtime::<()>::new();
        let mut opening = runtime.runtime_orchestrator().begin_open().unwrap();
        runtime.publish((), ());
        runtime.finish_open(&mut opening, Vec::new()).unwrap();

        let removal_attempt = runtime
            .runtime_orchestrator()
            .begin_final_removal()
            .unwrap();
        runtime.wait_for_returns();
        let subscriptions_stopped = runtime.close_subscriptions().unwrap();
        runtime.shutdown_handle_topics().unwrap();
        let sealed = runtime
            .seal_generation_services(subscriptions_stopped)
            .unwrap();
        runtime.finish_generation_services(sealed).unwrap();
        let module_epoch = runtime
            .lifecycle
            .access()
            .module_epoch_id()
            .expect("open module epoch");
        let _rtd = crate::excel_rtd::wait_for_module_quiescence().expect("RTD module quiescence");
        let removal_attempt =
            match removal_attempt.certify::<FinalRemoval>(QuiescenceProof::for_test(
                Some(crate::generation::RuntimeGeneration::new(1).unwrap()),
                module_epoch,
            )) {
                Err((_error, owner)) => owner,
                Ok(_certificate) => panic!("quiescence certificate must reject a live generation"),
            };
        assert_eq!(runtime.phase(), LifecyclePhase::Closing);

        assert!(runtime.take_current_generation().is_some());
        finish_test_close(&runtime, removal_attempt);
        assert_eq!(runtime.phase(), LifecyclePhase::Closed);
    }

    #[test]
    fn close_rejects_new_calls_and_waits_for_existing_call() {
        let _test_guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let runtime = Arc::new(Runtime::<TestU32Addin>::new());
        let mut open_attempt = runtime.runtime_orchestrator().begin_open().unwrap();
        runtime.publish(7_u32, ());
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();

        let ingress = crate::module_runtime::ingress()
            .enter_with(|| {})
            .into_admitted()
            .expect("test call enters during OPEN");
        let guard = runtime.enter(&ingress).unwrap();
        assert!(runtime.runtime_orchestrator().begin_close());
        crate::module_runtime::ingress().begin_close_with(|| {});
        assert!(matches!(runtime.enter(&ingress), Err(XllError::Closing)));

        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            let _ = crate::module_runtime::ingress().seal_and_drain();
            sender.send(()).unwrap();
        });

        assert!(receiver.recv_timeout(Duration::from_millis(20)).is_err());
        drop(guard);
        drop(ingress);
        receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn registration_storage_is_replaceable() {
        let runtime = Runtime::<()>::new();
        let mut journal = crate::registration::HostMutationJournal::default();
        journal
            .pending_registrations
            .push(crate::registration::PendingRegistration::from(
                RegistrationId {
                    id: 1.0,
                    excel_name: "TEST",
                },
            ));
        runtime.host.merge(journal);
        assert_eq!(runtime.host.registrations_snapshot().len(), 1);
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn module_residency_is_independent_from_logical_close() {
        let runtime = Runtime::<()>::new();
        assert!(!runtime.module_residency_held());
        assert!(runtime.ensure_module_residency(std::ptr::null()).unwrap());
        assert!(runtime.module_residency_held());
        assert!(!runtime.ensure_module_residency(std::ptr::null()).unwrap());

        runtime.runtime_orchestrator().quarantine();
        assert_eq!(runtime.phase(), LifecyclePhase::Quarantined);
        assert!(runtime.module_residency_held());
        runtime.release_module_residency().unwrap();
        assert!(!runtime.module_residency_held());
    }

    #[test]
    fn metadata_debt_storage_is_queryable() {
        let runtime = Runtime::<()>::new();
        runtime.host.retain_metadata_debt(vec![
            crate::registration::MetadataDebt::new(
                RegistrationId {
                    id: 1.0,
                    excel_name: "TEST_DEBT",
                },
                XllError::Closing,
            ),
            crate::registration::MetadataDebt::new(
                RegistrationId {
                    id: 2.0,
                    excel_name: "test_debt",
                },
                XllError::Panic,
            ),
        ]);
        assert_eq!(runtime.host.metadata_debt_snapshot().len(), 1);
        assert_eq!(
            runtime
                .host
                .metadata_debt_snapshot()
                .values()
                .next()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            runtime
                .host
                .metadata_debt_snapshot()
                .values()
                .next()
                .unwrap()[0]
                .expected_registration_id(),
            1.0
        );
        runtime
            .host
            .clear_metadata_debt_for_registrations(&[RegistrationId {
                id: 1.0,
                excel_name: "Test_Debt",
            }]);
        assert!(runtime.host.metadata_debt_snapshot().is_empty());
    }

    #[cfg(feature = "async")]
    #[test]
    fn calculation_end_advances_the_async_task_generation() {
        let _test_guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let runtime = Runtime::<()>::new();
        runtime.start_async(1).unwrap();
        let first = runtime.calculation_id().get();
        let (first_source, first_token) = crate::cancellation::CancellationSource::new(
            crate::cancellation::CancellationGuarantee::CalculationScoped,
        );
        runtime
            .async_manager()
            .spawn(first, std::future::pending(), first_source)
            .unwrap();

        runtime.finish_calculation();
        let second = runtime.calculation_id().get();
        assert_eq!(second, first + 1);
        assert!(matches!(
            runtime.async_manager().spawn(
                first,
                std::future::pending(),
                crate::cancellation::CancellationSource::new(
                    crate::cancellation::CancellationGuarantee::CalculationScoped,
                )
                .0,
            ),
            Err(XllError::ExcelValue(crate::ExcelError::NotAvailable))
        ));

        let (second_source, second_token) = crate::cancellation::CancellationSource::new(
            crate::cancellation::CancellationGuarantee::CalculationScoped,
        );
        runtime
            .async_manager()
            .spawn(second, std::future::pending(), second_source)
            .unwrap();
        runtime.cancel_async();
        assert!(second_token.is_cancelled());
        assert!(!first_token.is_cancelled());

        assert!(runtime.close_async().issues.is_empty());
        assert!(first_token.is_cancelled());
    }

    #[cfg(feature = "async")]
    #[test]
    fn published_async_generation_already_has_a_registry_entry() {
        let _test_guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let runtime = Arc::new(Runtime::<()>::new());
        runtime.start_async(1).unwrap();
        let first = runtime.calculation_id().get();
        let (published_tx, published_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let release_rx = Arc::new(std::sync::Mutex::new(release_rx));
        runtime
            .async_manager()
            .set_after_generation_publish_hook(Some(Arc::new(move || {
                published_tx.send(()).unwrap();
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(1))
                    .unwrap();
            })));

        let advancing_runtime = Arc::clone(&runtime);
        let advancing = thread::spawn(move || advancing_runtime.finish_calculation());
        published_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let published = runtime.calculation_id().get();
        assert_eq!(published, first + 1);
        let (spawned_tx, spawned_rx) = mpsc::sync_channel(1);
        let spawning_runtime = Arc::clone(&runtime);
        let spawning = thread::spawn(move || {
            let source = crate::cancellation::CancellationSource::new(
                crate::cancellation::CancellationGuarantee::CalculationScoped,
            )
            .0;
            spawned_tx
                .send(
                    spawning_runtime
                        .async_manager()
                        .spawn(published, async {}, source),
                )
                .unwrap();
        });

        let spawn_result = spawned_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("spawn should not wait for the manager state mutex");
        assert!(spawn_result.is_ok());
        release_tx.send(()).unwrap();
        advancing.join().unwrap();
        spawning.join().unwrap();

        runtime
            .async_manager()
            .set_after_generation_publish_hook(None);
        assert!(runtime.close_async().issues.is_empty());
    }
}
