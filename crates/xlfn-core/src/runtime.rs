use crate::execution::SharedUdfLayers;
use crate::{RegistrationId, XllError, XllResult};
use arc_swap::ArcSwapOption;
#[cfg(test)]
use parking_lot::MutexGuard;
use parking_lot::{Condvar, Mutex};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum LifecyclePhase {
    Closed = 0,
    Opening = 1,
    Open = 2,
    Closing = 3,
    OpenRollbackPending = 4,
}

impl LifecyclePhase {
    fn from_raw(value: u8) -> Self {
        match value {
            1 => Self::Opening,
            2 => Self::Open,
            3 => Self::Closing,
            4 => Self::OpenRollbackPending,
            _ => Self::Closed,
        }
    }
}

pub struct Runtime<S> {
    phase: AtomicU8,
    next_lifecycle_attempt: AtomicU64,
    generation: AtomicU64,
    open_attempt_id: AtomicU64,
    close_epoch: AtomicU64,
    state: ArcSwapOption<S>,
    layers: ArcSwapOption<SharedUdfLayers>,
    registrations: Mutex<Vec<crate::registration::PendingRegistration>>,
    metadata_debt:
        Mutex<BTreeMap<crate::registration::ExcelNameKey, Vec<crate::registration::MetadataDebt>>>,
    event_registrations: Mutex<Vec<crate::registration::EventRegistration>>,
    returns: OnceLock<Arc<crate::return_value::ReturnTracker>>,
    next_call_id: AtomicU64,
    #[cfg(not(feature = "async"))]
    calculation_id: AtomicU64,
    wait_lock: Mutex<()>,
    lifecycle_changed: Condvar,
    close_attempt_active: AtomicBool,
    registration_state_unknown: AtomicBool,
    handles: Mutex<Option<XllResult<Arc<crate::handle::HandleRuntime>>>>,
    subscriptions: Mutex<Option<Arc<crate::subscription::SubscriptionRuntime>>>,
    rtd_limits: crate::subscription::RtdLimits,
    #[cfg(feature = "async")]
    async_manager: crate::async_udf::AsyncManager,
    #[cfg(any(test, feature = "shutdown-refinement"))]
    ghost: OnceLock<crate::shutdown_refinement::GhostHandle>,
    #[cfg(test)]
    test_module_lease: Mutex<Option<crate::ingress::TestModuleLease>>,
}

impl<S> Runtime<S> {
    #[must_use]
    pub const fn new() -> Self {
        Self::new_with_rtd_limits(crate::subscription::RtdLimits::standard())
    }

    #[must_use]
    pub const fn new_with_rtd_limits(rtd_limits: crate::subscription::RtdLimits) -> Self {
        Self {
            phase: AtomicU8::new(LifecyclePhase::Closed as u8),
            next_lifecycle_attempt: AtomicU64::new(1),
            generation: AtomicU64::new(0),
            open_attempt_id: AtomicU64::new(0),
            close_epoch: AtomicU64::new(0),
            state: ArcSwapOption::const_empty(),
            layers: ArcSwapOption::const_empty(),
            registrations: Mutex::new(Vec::new()),
            metadata_debt: Mutex::new(BTreeMap::new()),
            event_registrations: Mutex::new(Vec::new()),
            returns: OnceLock::new(),
            next_call_id: AtomicU64::new(1),
            #[cfg(not(feature = "async"))]
            calculation_id: AtomicU64::new(1),
            wait_lock: Mutex::new(()),
            lifecycle_changed: Condvar::new(),
            close_attempt_active: AtomicBool::new(false),
            registration_state_unknown: AtomicBool::new(false),
            handles: Mutex::new(None),
            subscriptions: Mutex::new(None),
            rtd_limits,
            #[cfg(feature = "async")]
            async_manager: crate::async_udf::AsyncManager::new(),
            #[cfg(any(test, feature = "shutdown-refinement"))]
            ghost: OnceLock::new(),
            #[cfg(test)]
            test_module_lease: Mutex::new(None),
        }
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    fn ghost_handle(&self) -> crate::shutdown_refinement::GhostHandle {
        Arc::clone(
            self.ghost
                .get_or_init(|| Arc::new(crate::shutdown_refinement::ShutdownGhost::new())),
        )
    }

    #[must_use]
    pub fn phase(&self) -> LifecyclePhase {
        LifecyclePhase::from_raw(self.phase.load(Ordering::Acquire))
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    pub(crate) fn begin_open_if_epoch(
        &self,
        expected_close_epoch: u64,
    ) -> XllResult<OpenAttemptGuard<'_, S>> {
        #[cfg(test)]
        let test_module_lease = crate::ingress::acquire_test_module_lease();
        let _wait_guard = self.wait_lock.lock();
        if self.close_epoch.load(Ordering::Acquire) != expected_close_epoch
            || self.phase() != LifecyclePhase::Closed
            || self.open_attempt_id.load(Ordering::Acquire) != 0
            || self.close_attempt_active.load(Ordering::Acquire)
        {
            return Err(XllError::Internal {
                diagnostic_id: 0x4f50_454e_5048_4153,
            });
        }

        let attempt_id = self.next_lifecycle_attempt_id();
        let tracker = self.return_tracker();
        tracker.reopen_admission()?;

        crate::rtd::begin_module_open();
        crate::callback_gate::reset_from_runtime();
        crate::ingress::global_ingress().begin_opening();
        #[cfg(test)]
        {
            *self.test_module_lease.lock() = Some(test_module_lease);
        }
        self.open_attempt_id.store(attempt_id, Ordering::Release);
        self.phase
            .store(LifecyclePhase::Opening as u8, Ordering::Release);
        Ok(OpenAttemptGuard {
            runtime: self,
            attempt_id,
            active: true,
        })
    }

    #[cfg(test)]
    pub(crate) fn begin_open(&self) -> XllResult<OpenAttemptGuard<'_, S>> {
        self.begin_open_if_epoch(self.close_epoch())
    }

    fn next_lifecycle_attempt_id(&self) -> u64 {
        loop {
            let attempt_id = self.next_lifecycle_attempt.fetch_add(1, Ordering::Relaxed);
            if attempt_id != 0 {
                return attempt_id;
            }
        }
    }

    pub(crate) fn close_epoch(&self) -> u64 {
        self.close_epoch.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn publish(&self, state: S, layers: Vec<Arc<dyn crate::UdfLayer>>) {
        self.publish_state(state);
        self.publish_layers(layers);
    }

    pub(crate) fn publish_state(&self, state: S) {
        self.state.store(Some(Arc::new(state)));
    }

    pub(crate) fn opening_state(&self) -> Option<Arc<S>> {
        self.state.load_full()
    }

    pub(crate) fn publish_layers(&self, layers: Vec<Arc<dyn crate::UdfLayer>>) {
        if layers.is_empty() {
            self.layers.store(None);
        } else {
            self.layers.store(Some(Arc::new(layers)));
        }
    }

    pub(crate) fn finish_open(
        &self,
        attempt: &mut OpenAttemptGuard<'_, S>,
        registrations: Vec<RegistrationId>,
    ) -> XllResult<()> {
        let _wait_guard = self.wait_lock.lock();
        self.lifecycle_changed.notify_all();
        if self.open_attempt_id.load(Ordering::Acquire) != attempt.attempt_id {
            attempt.active = false;
            return Err(XllError::Closing);
        }

        // Once this attempt owns the lifecycle slot, retain every host
        // registration even when a concurrent close has already won the phase
        // transition. The close owner needs those IDs to unregister the host
        // mutations before publishing Closed.
        self.clear_metadata_debt_for_registrations(&registrations);
        let new_items: Vec<_> = registrations
            .into_iter()
            .map(crate::registration::PendingRegistration::from)
            .collect();
        self.registrations.lock().extend(new_items);
        let can_commit = self.phase() == LifecyclePhase::Opening;
        if can_commit {
            let ingress = crate::ingress::global_ingress();
            #[cfg(any(test, feature = "shutdown-refinement"))]
            {
                let ghost = self.ghost_handle();
                ingress
                    .complete_open(|| {
                        let mut resources = crate::shutdown_refinement::GhostResources::opened(
                            self.registrations.lock().len() as u64,
                            self.event_registrations.lock().len() as u64,
                        );
                        #[cfg(feature = "async")]
                        {
                            resources.async_executor_running = !self.async_manager.is_stopped();
                        }
                        crate::diagnostics::connect_ghost(Arc::clone(&ghost), |snapshot| {
                            resources.diagnostics_running = snapshot.running;
                            resources.diagnostics_pending = snapshot.pending;
                            ghost
                                .begin_generation(attempt.attempt_id, resources)
                                .map_err(|_| XllError::Internal {
                                    diagnostic_id: 0x4748_4f53_5447_454e,
                                })
                        })?;
                        crate::rtd::set_ghost(Arc::clone(&ghost));
                        if let Some(tracker) = self.returns.get() {
                            tracker.set_ghost(Arc::clone(&ghost));
                        }
                        if let Some(Ok(handles)) = self.handles.lock().as_ref() {
                            handles.set_ghost(Arc::clone(&ghost));
                        }
                        if let Some(subscriptions) = self.subscriptions.lock().as_ref() {
                            subscriptions.set_ghost(Arc::clone(&ghost));
                        }
                        #[cfg(feature = "async")]
                        self.async_manager.set_ghost(Arc::clone(&ghost));
                        self.phase
                            .store(LifecyclePhase::Open as u8, Ordering::Release);
                        self.generation.store(attempt.attempt_id, Ordering::Release);
                        Ok(())
                    })
                    .map_err(|_| XllError::Closing)??;
            }
            #[cfg(not(any(test, feature = "shutdown-refinement")))]
            ingress
                .complete_open(|| {
                    self.phase
                        .store(LifecyclePhase::Open as u8, Ordering::Release);
                    self.generation.store(attempt.attempt_id, Ordering::Release);
                    Ok::<(), XllError>(())
                })
                .map_err(|_| XllError::Closing)??;
        }
        self.open_attempt_id.store(0, Ordering::Release);
        attempt.active = false;

        if can_commit {
            Ok(())
        } else {
            Err(XllError::Closing)
        }
    }

    #[cfg(feature = "async")]
    pub(crate) fn set_event_registrations(
        &self,
        registrations: Vec<crate::registration::EventRegistration>,
    ) {
        self.event_registrations.lock().extend(registrations);
    }

    pub(crate) fn retain_registration_debt(
        &self,
        registrations: Vec<crate::registration::PendingRegistration>,
    ) {
        self.registrations.lock().extend(registrations);
    }

    pub(crate) fn retain_event_registration_debt(
        &self,
        registrations: Vec<crate::registration::EventRegistration>,
    ) {
        self.event_registrations.lock().extend(registrations);
    }

    pub(crate) fn mark_registration_state_unknown(&self) {
        self.registration_state_unknown
            .store(true, Ordering::Release);
    }

    pub(crate) fn registration_state_unknown(&self) -> bool {
        self.registration_state_unknown.load(Ordering::Acquire)
    }

    fn fail_open_attempt(&self, attempt_id: u64) -> bool {
        let _wait_guard = self.wait_lock.lock();
        if self.open_attempt_id.load(Ordering::Acquire) != attempt_id {
            return false;
        }

        self.open_attempt_id.store(0, Ordering::Release);
        let should_rollback = match self.phase() {
            LifecyclePhase::Opening => {
                if let Some(tracker) = self.returns.get() {
                    tracker.close_admission();
                }
                self.phase
                    .store(LifecyclePhase::OpenRollbackPending as u8, Ordering::Release);
                true
            }
            LifecyclePhase::OpenRollbackPending => true,
            LifecyclePhase::Closed | LifecyclePhase::Open | LifecyclePhase::Closing => false,
        };
        self.lifecycle_changed.notify_all();
        should_rollback
    }

    pub fn enter(&self) -> XllResult<CallGuard<'_, S>> {
        let concurrent_calls = crate::ingress::global_ingress().active_udfs();
        crate::ingress::global_ingress().with_linearization(|| {
            if self.phase() != LifecyclePhase::Open {
                return Err(XllError::Closing);
            }

            let state = self.state.load_full();
            match state {
                Some(state) => {
                    #[cfg(any(test, feature = "shutdown-refinement"))]
                    self.record_ghost_event(crate::shutdown_refinement::GhostEvent::EnterCall);
                    Ok(CallGuard {
                        runtime: self,
                        state,
                        concurrent_calls,
                    })
                }
                None => Err(XllError::Internal {
                    diagnostic_id: 0x4d49_5353_5354_4154,
                }),
            }
        })
    }

    #[cfg(test)]
    pub(crate) fn begin_close(&self) -> bool {
        let _wait_guard = self.wait_lock.lock();
        crate::ingress::global_ingress().with_linearization(|| {
            if self.phase() == LifecyclePhase::Open {
                if let Some(tracker) = self.returns.get() {
                    tracker.close_admission();
                }
                self.phase
                    .store(LifecyclePhase::Closing as u8, Ordering::Release);
                true
            } else {
                false
            }
        })
    }

    pub(crate) fn begin_final_close(&self) -> Option<CloseAttemptGuard<'_, S>> {
        let mut wait_guard = self.wait_lock.lock();
        // Every final-close invocation invalidates open operations that started
        // before it, including an operation that is between rollback recovery
        // and acquisition of its open-attempt token while the phase is Closed.
        self.close_epoch.fetch_add(1, Ordering::AcqRel);
        if let Some(tracker) = self.returns.get() {
            tracker.close_admission();
        }
        loop {
            let decision = crate::ingress::global_ingress().with_linearization(|| {
                match self.phase() {
                    LifecyclePhase::Closed => {
                        // A cleanup owner publishes Closed before its guard leaves
                        // the callback stack. A concurrent xlAutoClose must not
                        // return until that owner has fully exited, because Excel
                        // may unload the XLL immediately afterwards.
                        if !self.close_attempt_active.load(Ordering::Acquire)
                            && self.returns_are_quiescent()
                        {
                            return Some(false);
                        }
                        if !self.close_attempt_active.load(Ordering::Acquire) {
                            self.phase
                                .store(LifecyclePhase::Closing as u8, Ordering::Release);
                        }
                    }
                    LifecyclePhase::Closing => {}
                    LifecyclePhase::Opening
                    | LifecyclePhase::Open
                    | LifecyclePhase::OpenRollbackPending => {
                        self.phase
                            .store(LifecyclePhase::Closing as u8, Ordering::Release);
                    }
                }

                if self.phase() != LifecyclePhase::Closed
                    && self.open_attempt_id.load(Ordering::Acquire) == 0
                    && !self.close_attempt_active.load(Ordering::Acquire)
                {
                    self.close_attempt_active.store(true, Ordering::Release);
                    Some(true)
                } else {
                    None
                }
            });
            match decision {
                Some(true) => return Some(CloseAttemptGuard { runtime: self }),
                Some(false) => return None,
                None => self.lifecycle_changed.wait(&mut wait_guard),
            }
        }
    }

    pub(crate) fn acquire_open_rollback(&self) -> Option<CloseAttemptGuard<'_, S>> {
        let mut wait_guard = self.wait_lock.lock();
        loop {
            match self.phase() {
                LifecyclePhase::Closed => return None,
                LifecyclePhase::OpenRollbackPending => {}
                LifecyclePhase::Closing | LifecyclePhase::Opening | LifecyclePhase::Open => {
                    return None;
                }
            }
            if !self.close_attempt_active.load(Ordering::Acquire) {
                self.close_attempt_active.store(true, Ordering::Release);
                return Some(CloseAttemptGuard { runtime: self });
            }
            self.lifecycle_changed.wait(&mut wait_guard);
        }
    }

    pub(crate) fn registrations(&self) -> Vec<crate::registration::PendingRegistration> {
        self.registrations.lock().clone()
    }

    pub(crate) fn retain_failed_registrations(
        &self,
        failed: Vec<(crate::registration::PendingRegistration, XllError)>,
    ) {
        *self.registrations.lock() = failed.into_iter().map(|(entry, _)| entry).collect();
    }

    pub(crate) fn retain_metadata_debt(
        &self,
        metadata_debt: Vec<crate::registration::MetadataDebt>,
    ) {
        let mut retained = self.metadata_debt.lock();
        for debt in metadata_debt {
            let key = debt.key();
            retained.entry(key).or_default().push(debt);
        }
    }

    pub(crate) fn metadata_debt(
        &self,
    ) -> BTreeMap<crate::registration::ExcelNameKey, Vec<crate::registration::MetadataDebt>> {
        self.metadata_debt.lock().clone()
    }

    pub(crate) fn clear_metadata_debt_for_registrations(&self, registrations: &[RegistrationId]) {
        let mut debts = self.metadata_debt.lock();
        for registration in registrations {
            debts.remove(&crate::registration::ExcelNameKey::new(
                registration.excel_name,
            ));
        }
    }

    pub(crate) fn replace_metadata_debt(
        &self,
        debts: BTreeMap<crate::registration::ExcelNameKey, Vec<crate::registration::MetadataDebt>>,
    ) {
        *self.metadata_debt.lock() = debts;
    }

    pub(crate) fn has_metadata_debt(&self) -> bool {
        !self.metadata_debt.lock().is_empty()
    }

    pub(crate) fn event_registrations(&self) -> Vec<crate::registration::EventRegistration> {
        self.event_registrations.lock().clone()
    }

    pub(crate) fn retain_failed_event_registrations(
        &self,
        failed: Vec<(crate::registration::EventRegistration, XllError)>,
    ) {
        *self.event_registrations.lock() = failed.into_iter().map(|(entry, _)| entry).collect();
    }

    pub(crate) fn return_tracker(&self) -> &Arc<crate::return_value::ReturnTracker> {
        self.returns.get_or_init(|| {
            let tracker = Arc::new(crate::return_value::ReturnTracker::new_closed());
            #[cfg(any(test, feature = "shutdown-refinement"))]
            if self.ghost_handle().active() {
                tracker.set_ghost(self.ghost_handle());
            }
            tracker
        })
    }

    pub(crate) fn enter_return_producer(&self) -> Option<crate::return_value::ReturnProducerGuard> {
        self.returns
            .get()
            .and_then(|tracker| tracker.try_enter_producer())
    }

    pub(crate) fn wait_for_returns(&self) {
        if let Some(tracker) = self.returns.get() {
            tracker.wait_for_quiescence();
        }
    }

    pub(crate) fn returns_are_quiescent(&self) -> bool {
        self.returns
            .get()
            .is_none_or(|tracker| tracker.is_quiescent())
    }

    fn returns_closed_and_quiescent(&self) -> bool {
        self.returns
            .get()
            .is_none_or(|tracker| tracker.admission_closed() && tracker.is_quiescent())
    }

    pub(crate) fn take_state(&self) -> Option<Arc<S>> {
        self.state.swap(None)
    }

    pub(crate) fn restore_state_arc(&self, state: Arc<S>) {
        self.state.store(Some(state));
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn record_ghost_event(&self, event: crate::shutdown_refinement::GhostEvent) {
        self.ghost_handle().record_event(event);
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn record_ghost_event_linearized(
        &self,
        event: crate::shutdown_refinement::GhostEvent,
    ) {
        crate::ingress::global_ingress().with_linearization(|| self.record_ghost_event(event));
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn record_ghost_state_unique(&self) {
        self.record_ghost_event(crate::shutdown_refinement::GhostEvent::ProveStateUnique);
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn record_ghost_addin_quiesced(&self) {
        self.record_ghost_event(crate::shutdown_refinement::GhostEvent::ProveAddinQuiesced);
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn record_ghost_async_stopped(&self) {
        let ghost = self.ghost_handle();
        if ghost.state().resources.async_executor_running {
            self.record_ghost_event(crate::shutdown_refinement::GhostEvent::StopAsyncExecutor);
        }
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn record_ghost_diagnostics_stopped(&self) -> XllResult<()> {
        crate::diagnostics::record_ghost_diagnostics_stopped(self.ghost_handle())
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn ghost_fail_stop(&self, reason: crate::shutdown_refinement::GhostFailure) {
        if let Err(violation) = self.ghost_handle().fail_stop(reason) {
            tracing::error!(%violation, "shutdown ghost fail-stop recording failed");
        }
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn ghost_generation_active(&self) -> bool {
        self.ghost_handle().active()
    }

    #[cfg(test)]
    pub(crate) fn disable_ghost_for_test(&self) {
        self.ghost_handle().disable_for_test();
    }

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn record_ghost_returned_success(&self, witness: ClosedWitness) -> XllResult<()> {
        if witness.runtime_address != std::ptr::from_ref(self).addr()
            || witness.generation != self.generation()
            || self.phase() != LifecyclePhase::Closed
        {
            return Err(XllError::Internal {
                diagnostic_id: 0x434c_4f53_5754_4e4f,
            });
        }
        if self.ghost_handle().active() {
            self.ghost_handle()
                .record_returned_success()
                .map_err(|_| XllError::Internal {
                    diagnostic_id: 0x434c_4f53_5452_5355,
                })?;
        }
        Ok(())
    }

    #[cfg(any(test, feature = "shutdown-trace"))]
    #[allow(
        dead_code,
        reason = "Trace extraction is consumed by the feature-gated Lean checker test"
    )]
    pub(crate) fn ghost_trace_json(&self) -> String {
        self.ghost_handle()
            .trace_json()
            .expect("ghost trace serialization")
    }
}

#[derive(Debug)]
pub struct CloseCertificate {
    #[allow(
        dead_code,
        reason = "Typestate proof token for linear lifecycle release"
    )]
    pub(crate) exports: crate::ingress::ExportsDrained,
    #[allow(
        dead_code,
        reason = "Typestate proof token for linear lifecycle release"
    )]
    pub(crate) rtd: crate::rtd::RtdQuiescent,
    #[allow(
        dead_code,
        reason = "Typestate proof token for linear lifecycle release"
    )]
    pub(crate) host_callbacks: crate::shutdown::HostCallbacksDetached,
    #[allow(
        dead_code,
        reason = "Typestate proof token for linear lifecycle release"
    )]
    pub(crate) async_stopped: crate::shutdown::AsyncStopped,
    #[allow(
        dead_code,
        reason = "Typestate proof token for linear lifecycle release"
    )]
    pub(crate) subscriptions_stopped: crate::shutdown::SubscriptionsStopped,
    #[allow(
        dead_code,
        reason = "Typestate proof token for linear lifecycle release"
    )]
    pub(crate) handles_quiescent: crate::shutdown::HandlesQuiescent,
    #[allow(
        dead_code,
        reason = "Typestate proof token for linear lifecycle release"
    )]
    pub(crate) diagnostics_stopped: crate::diagnostics::DiagnosticsStopped,
    #[allow(
        dead_code,
        reason = "Typestate proof token for linear lifecycle release"
    )]
    pub(crate) addin_quiesced: crate::shutdown::AddinQuiesced,
    runtime_address: usize,
    close_attempt_id: u64,
    generation: u64,
}

#[derive(Debug)]
pub(crate) struct ClosedWitness {
    #[cfg(any(test, feature = "shutdown-refinement"))]
    runtime_address: usize,
    #[cfg(any(test, feature = "shutdown-refinement"))]
    generation: u64,
}

#[derive(Debug)]
pub(crate) struct OpenRollbackCertificate {
    #[allow(
        dead_code,
        reason = "Typestate proof token for linear lifecycle release"
    )]
    pub(crate) exports: crate::ingress::ExportsDrained,
    #[allow(
        dead_code,
        reason = "Typestate proof token for linear lifecycle release"
    )]
    pub(crate) rtd: crate::rtd::RtdQuiescent,
    #[allow(
        dead_code,
        reason = "Typestate proof token for linear lifecycle release"
    )]
    pub(crate) host_callbacks: crate::shutdown::HostCallbacksDetached,
    #[allow(
        dead_code,
        reason = "Typestate proof token for linear lifecycle release"
    )]
    pub(crate) async_stopped: crate::shutdown::AsyncStopped,
    #[allow(
        dead_code,
        reason = "Typestate proof token for linear lifecycle release"
    )]
    pub(crate) subscriptions_stopped: crate::shutdown::SubscriptionsStopped,
    #[allow(
        dead_code,
        reason = "Typestate proof token for linear lifecycle release"
    )]
    pub(crate) handles_quiescent: crate::shutdown::HandlesQuiescent,
    #[allow(
        dead_code,
        reason = "Typestate proof token for linear lifecycle release"
    )]
    pub(crate) diagnostics_stopped: crate::diagnostics::DiagnosticsStopped,
    #[allow(
        dead_code,
        reason = "Typestate proof token for linear lifecycle release"
    )]
    pub(crate) addin_quiesced: crate::shutdown::AddinQuiesced,
    runtime_address: usize,
}

pub(crate) struct ClosePrerequisites {
    pub(crate) exports: crate::ingress::ExportsDrained,
    pub(crate) rtd: crate::rtd::RtdQuiescent,
    pub(crate) host_callbacks: crate::shutdown::HostCallbacksDetached,
    pub(crate) async_stopped: crate::shutdown::AsyncStopped,
    pub(crate) subscriptions_stopped: crate::shutdown::SubscriptionsStopped,
    pub(crate) handles_quiescent: crate::shutdown::HandlesQuiescent,
    pub(crate) diagnostics_stopped: crate::diagnostics::DiagnosticsStopped,
    pub(crate) addin_quiesced: crate::shutdown::AddinQuiesced,
}

pub(crate) struct OpenRollbackPrerequisites {
    pub(crate) exports: crate::ingress::ExportsDrained,
    pub(crate) rtd: crate::rtd::RtdQuiescent,
    pub(crate) host_callbacks: crate::shutdown::HostCallbacksDetached,
    pub(crate) async_stopped: crate::shutdown::AsyncStopped,
    pub(crate) subscriptions_stopped: crate::shutdown::SubscriptionsStopped,
    pub(crate) handles_quiescent: crate::shutdown::HandlesQuiescent,
    pub(crate) diagnostics_stopped: crate::diagnostics::DiagnosticsStopped,
    pub(crate) addin_quiesced: crate::shutdown::AddinQuiesced,
}

impl<S> Runtime<S> {
    pub(crate) fn certify_open_rollback(
        &self,
        prerequisites: OpenRollbackPrerequisites,
    ) -> XllResult<OpenRollbackCertificate> {
        let _wait_guard = self.wait_lock.lock();
        let services_stopped = self.handles.lock().is_none() && self.subscriptions.lock().is_none();
        #[cfg(feature = "async")]
        let async_stopped = self.async_manager.is_stopped();
        #[cfg(not(feature = "async"))]
        let async_stopped = true;

        let certified = matches!(
            self.phase(),
            LifecyclePhase::OpenRollbackPending | LifecyclePhase::Closing
        ) && self.open_attempt_id.load(Ordering::Acquire) == 0
            && self.close_attempt_active.load(Ordering::Acquire)
            && self.returns_closed_and_quiescent()
            && async_stopped
            && services_stopped
            && self.state.load_full().is_none()
            && self.registrations.lock().is_empty()
            && self.event_registrations.lock().is_empty()
            && !self.registration_state_unknown();

        if !certified {
            return Err(XllError::Internal {
                diagnostic_id: 0x4f50_5242_4345_5254,
            });
        }

        Ok(OpenRollbackCertificate {
            exports: prerequisites.exports,
            rtd: prerequisites.rtd,
            host_callbacks: prerequisites.host_callbacks,
            async_stopped: prerequisites.async_stopped,
            subscriptions_stopped: prerequisites.subscriptions_stopped,
            handles_quiescent: prerequisites.handles_quiescent,
            diagnostics_stopped: prerequisites.diagnostics_stopped,
            addin_quiesced: prerequisites.addin_quiesced,
            runtime_address: std::ptr::from_ref(self).addr(),
        })
    }

    pub(crate) fn finish_open_rollback(
        &self,
        certificate: OpenRollbackCertificate,
    ) -> XllResult<()> {
        if certificate.runtime_address != std::ptr::from_ref(self).addr() {
            return Err(XllError::Internal {
                diagnostic_id: 0x4f50_5242_4345_5255,
            });
        }
        self.layers.store(None);
        let _wait_guard = self.wait_lock.lock();
        debug_assert_eq!(self.open_attempt_id.load(Ordering::Acquire), 0);
        if !matches!(
            self.phase(),
            LifecyclePhase::OpenRollbackPending | LifecyclePhase::Closing
        ) {
            return Err(XllError::Internal {
                diagnostic_id: 0x4f50_5242_5048_4153,
            });
        }
        self.phase
            .store(LifecyclePhase::Closed as u8, Ordering::Release);
        crate::callback_gate::close_from_runtime();
        self.lifecycle_changed.notify_all();
        crate::rtd::certify_module_unload();
        #[cfg(test)]
        drop(self.test_module_lease.lock().take());
        Ok(())
    }

    pub(crate) fn certify_close(
        &self,
        prerequisites: ClosePrerequisites,
    ) -> XllResult<CloseCertificate> {
        let _wait_guard = self.wait_lock.lock();
        let services_stopped = self.handles.lock().is_none() && self.subscriptions.lock().is_none();
        #[cfg(feature = "async")]
        let async_stopped = self.async_manager.is_stopped();
        #[cfg(not(feature = "async"))]
        let async_stopped = true;

        let certified = self.phase() == LifecyclePhase::Closing
            && self.open_attempt_id.load(Ordering::Acquire) == 0
            && self.close_attempt_active.load(Ordering::Acquire)
            && self.returns_closed_and_quiescent()
            && async_stopped
            && services_stopped
            && self.state.load_full().is_none()
            && self.registrations.lock().is_empty()
            && self.event_registrations.lock().is_empty()
            && !self.registration_state_unknown();

        if !certified {
            return Err(XllError::Internal {
                diagnostic_id: 0x434c_4f53_4543_4552,
            });
        }

        Ok(CloseCertificate {
            exports: prerequisites.exports,
            rtd: prerequisites.rtd,
            host_callbacks: prerequisites.host_callbacks,
            async_stopped: prerequisites.async_stopped,
            subscriptions_stopped: prerequisites.subscriptions_stopped,
            handles_quiescent: prerequisites.handles_quiescent,
            diagnostics_stopped: prerequisites.diagnostics_stopped,
            addin_quiesced: prerequisites.addin_quiesced,
            runtime_address: std::ptr::from_ref(self).addr(),
            close_attempt_id: self.close_epoch.load(Ordering::Acquire),
            generation: self.generation.load(Ordering::Acquire),
        })
    }

    pub(crate) fn finish_close(&self, certificate: CloseCertificate) -> XllResult<ClosedWitness> {
        if certificate.runtime_address != std::ptr::from_ref(self).addr() {
            return Err(XllError::Internal {
                diagnostic_id: 0x434c_4f53_4552_554e,
            });
        }
        if certificate.close_attempt_id != self.close_epoch.load(Ordering::Acquire) {
            return Err(XllError::Internal {
                diagnostic_id: 0x434c_4f53_4545,
            });
        }
        if certificate.generation != self.generation() {
            return Err(XllError::Internal {
                diagnostic_id: 0x434c_4c4f_5345_4745,
            });
        }
        self.layers.store(None);
        let _wait_guard = self.wait_lock.lock();
        #[cfg(any(test, feature = "shutdown-refinement"))]
        if self.ghost_handle().active() {
            self.ghost_handle()
                .apply(crate::shutdown_refinement::GhostEvent::FinishClose)
                .map_err(|_| XllError::Internal {
                    diagnostic_id: 0x434c_4f53_5447_484f,
                })?;
        }
        crate::callback_gate::close_from_runtime();
        self.phase
            .store(LifecyclePhase::Closed as u8, Ordering::Release);
        self.lifecycle_changed.notify_all();
        crate::rtd::certify_module_unload();
        #[cfg(test)]
        drop(self.test_module_lease.lock().take());
        Ok(ClosedWitness {
            #[cfg(any(test, feature = "shutdown-refinement"))]
            runtime_address: std::ptr::from_ref(self).addr(),
            #[cfg(any(test, feature = "shutdown-refinement"))]
            generation: certificate.generation,
        })
    }

    pub(crate) fn next_call_id(&self) -> u64 {
        self.next_call_id.fetch_add(1, Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn peek_next_call_id(&self) -> u64 {
        self.next_call_id.load(Ordering::Relaxed)
    }

    pub(crate) fn layers_if_configured(&self) -> Option<Arc<SharedUdfLayers>> {
        let layers = self.layers.load();
        match layers.as_ref() {
            Some(layers) if !layers.is_empty() => Some(Arc::clone(layers)),
            _ => None,
        }
    }

    pub(crate) fn calculation_id(&self) -> crate::CalculationId {
        #[cfg(feature = "async")]
        {
            self.async_manager.current_generation().into()
        }
        #[cfg(not(feature = "async"))]
        {
            self.calculation_id.load(Ordering::Acquire).into()
        }
    }

    #[cfg(feature = "async")]
    pub(crate) fn finish_calculation(&self) {
        let _ = self.async_manager.advance_generation();
    }

    pub(crate) fn handles(&self) -> XllResult<Arc<crate::handle::HandleRuntime>> {
        if let Some(handles) = self.handles.lock().as_ref() {
            let result = handles.as_ref().map(Arc::clone).map_err(Clone::clone);
            #[cfg(any(test, feature = "shutdown-refinement"))]
            if let (Some(ghost), Ok(handles)) = (self.ghost.get(), &result) {
                handles.set_ghost(Arc::clone(ghost));
            }
            return result;
        }

        // Entropy acquisition and failure diagnostics can invoke platform or
        // subscriber code. Keep them outside the runtime slot lock so a
        // diagnostic subscriber can safely re-enter runtime services.
        let mut candidate = Some(
            crate::handle::HandleRuntime::try_new_with_ingress(
                16_384,
                Some(crate::ingress::global_ingress()),
            )
            .map(Arc::new),
        );
        let result = {
            let mut slot = self.handles.lock();
            if slot.is_none() {
                *slot = candidate.take();
            }
            slot.as_ref()
                .expect("the handle runtime result was installed")
                .as_ref()
                .map(Arc::clone)
                .map_err(Clone::clone)
        };
        // A concurrent initializer may have won. Drop this empty candidate only
        // after releasing the slot lock.
        drop(candidate);
        #[cfg(any(test, feature = "shutdown-refinement"))]
        if let (Some(ghost), Ok(handles)) = (self.ghost.get(), &result) {
            handles.set_ghost(Arc::clone(ghost));
        }
        result
    }

    pub(crate) fn close_handles(&self) -> XllResult<crate::shutdown::HandlesQuiescent> {
        let handles = self.handles.lock().take();
        let result = if let Some(Ok(handles)) = handles {
            let rtd_result = crate::rtd::shutdown(Arc::clone(&handles));
            let handle_result = handles.close();
            rtd_result.and(handle_result)
        } else {
            Ok(())
        };
        result.map(|()| crate::shutdown::HandlesQuiescent::new())
    }

    pub(crate) fn subscriptions(&self) -> Arc<crate::subscription::SubscriptionRuntime> {
        let mut slot = self.subscriptions.lock();
        let subscriptions = slot.get_or_insert_with(|| {
            Arc::new(crate::subscription::SubscriptionRuntime::with_module_ingress(self.rtd_limits))
        });
        #[cfg(any(test, feature = "shutdown-refinement"))]
        if let Some(ghost) = self.ghost.get() {
            subscriptions.set_ghost(Arc::clone(ghost));
        }
        Arc::clone(subscriptions)
    }

    pub(crate) fn close_subscriptions(&self) -> XllResult<crate::shutdown::SubscriptionsStopped> {
        let subscriptions = self.subscriptions.lock().take();
        let result = if let Some(subscriptions) = subscriptions {
            crate::rtd::shutdown_subscriptions(subscriptions)
        } else {
            Ok(())
        };
        result.map(|()| crate::shutdown::SubscriptionsStopped::new())
    }

    #[cfg(feature = "async")]
    pub(crate) fn start_async(&self, worker_count: usize) -> XllResult<()> {
        self.async_manager.start(worker_count)
    }

    #[cfg(feature = "async")]
    pub(crate) fn cancel_async(&self) {
        self.async_manager.cancel_current_generation();
    }

    #[cfg(feature = "async")]
    pub(crate) fn close_async(
        &self,
    ) -> crate::shutdown::StopOutcome<crate::shutdown::AsyncStopped> {
        self.async_manager.close()
    }

    #[cfg(feature = "async")]
    pub(crate) fn async_manager(&self) -> &crate::async_udf::AsyncManager {
        &self.async_manager
    }

    #[cfg(test)]
    fn registrations_guard(&self) -> MutexGuard<'_, Vec<crate::registration::PendingRegistration>> {
        self.registrations.lock()
    }

    #[cfg(test)]
    pub(crate) fn release_test_module_lease(&self) {
        drop(self.test_module_lease.lock().take());
    }
}

impl<S> Default for Runtime<S> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
impl<S> Drop for Runtime<S> {
    fn drop(&mut self) {
        if !matches!(self.phase(), LifecyclePhase::Closed) {
            let ingress = crate::ingress::global_ingress();
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
        drop(self.test_module_lease.get_mut().take());
    }
}

pub(crate) struct CloseAttemptGuard<'runtime, S> {
    runtime: &'runtime Runtime<S>,
}

impl<S> Drop for CloseAttemptGuard<'_, S> {
    fn drop(&mut self) {
        let _wait_guard = self.runtime.wait_lock.lock();
        self.runtime
            .close_attempt_active
            .store(false, Ordering::Release);
        self.runtime.lifecycle_changed.notify_all();
    }
}

pub(crate) struct OpenAttemptGuard<'runtime, S> {
    runtime: &'runtime Runtime<S>,
    attempt_id: u64,
    active: bool,
}

impl<S> OpenAttemptGuard<'_, S> {
    pub(crate) const fn is_active(&self) -> bool {
        self.active
    }

    pub(crate) fn fail(&mut self) -> bool {
        if !self.active {
            return false;
        }
        let should_rollback = self.runtime.fail_open_attempt(self.attempt_id);
        self.active = false;
        should_rollback
    }
}

impl<S> Drop for OpenAttemptGuard<'_, S> {
    fn drop(&mut self) {
        if !self.active {
            return;
        }

        let _ = self.runtime.fail_open_attempt(self.attempt_id);
        self.active = false;
    }
}

pub struct CallGuard<'runtime, S> {
    #[allow(
        dead_code,
        reason = "Runtime reference used for shutdown refinement ghost event recording"
    )]
    runtime: &'runtime Runtime<S>,
    state: Arc<S>,
    concurrent_calls: usize,
}

impl<S> CallGuard<'_, S> {
    #[must_use]
    pub fn state(&self) -> &S {
        &self.state
    }

    #[must_use]
    pub const fn concurrent_calls(&self) -> usize {
        self.concurrent_calls
    }

    #[cfg(feature = "async")]
    #[must_use]
    pub(crate) fn state_arc(&self) -> Arc<S> {
        Arc::clone(&self.state)
    }
}

impl<S> Drop for CallGuard<'_, S> {
    fn drop(&mut self) {
        #[cfg(any(test, feature = "shutdown-refinement"))]
        self.runtime
            .record_ghost_event(crate::shutdown_refinement::GhostEvent::LeaveCall);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    fn finish_test_close<S>(runtime: &Runtime<S>) {
        let ingress = crate::ingress::global_ingress();
        if matches!(
            ingress.phase(),
            crate::ingress::PHASE_OPENING | crate::ingress::PHASE_OPEN
        ) {
            ingress.begin_close_with(|| {
                #[cfg(any(test, feature = "shutdown-refinement"))]
                if runtime.ghost_generation_active() {
                    runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::BeginClose);
                }
            });
        }
        let exports = ingress.seal_and_drain();
        // This helper validates Runtime's close certificate in isolation. It
        // deliberately does not synthesize lifecycle ghost milestones; those
        // are exercised by the real lifecycle close path.
        runtime.disable_ghost_for_test();
        let rtd = crate::rtd::wait_for_module_quiescence().expect("RTD module quiescence");
        let certificate = runtime
            .certify_close(ClosePrerequisites {
                exports,
                rtd,
                host_callbacks: crate::shutdown::HostCallbacksDetached::new(),
                async_stopped: crate::shutdown::AsyncStopped::new(),
                subscriptions_stopped: crate::shutdown::SubscriptionsStopped::new(),
                handles_quiescent: crate::shutdown::HandlesQuiescent::new(),
                diagnostics_stopped: crate::diagnostics::DiagnosticsStopped::for_test(),
                addin_quiesced: crate::shutdown::AddinQuiesced::new(),
            })
            .unwrap();
        runtime.finish_close(certificate).unwrap();
        runtime.release_test_module_lease();
    }

    fn finish_test_open_rollback<S>(runtime: &Runtime<S>) {
        let ingress = crate::ingress::global_ingress();
        if matches!(
            ingress.phase(),
            crate::ingress::PHASE_OPENING | crate::ingress::PHASE_OPEN
        ) {
            ingress.begin_close_with(|| {});
        }
        let exports = ingress.seal_and_drain();
        let certificate = runtime
            .certify_open_rollback(OpenRollbackPrerequisites {
                exports,
                rtd: crate::rtd::wait_for_module_quiescence().expect("RTD module quiescence"),
                host_callbacks: crate::shutdown::HostCallbacksDetached::new(),
                async_stopped: crate::shutdown::AsyncStopped::new(),
                subscriptions_stopped: crate::shutdown::SubscriptionsStopped::new(),
                handles_quiescent: crate::shutdown::HandlesQuiescent::new(),
                diagnostics_stopped: crate::diagnostics::DiagnosticsStopped::for_test(),
                addin_quiesced: crate::shutdown::AddinQuiesced::new(),
            })
            .unwrap();
        runtime.finish_open_rollback(certificate).unwrap();
        runtime.release_test_module_lease();
    }

    pub(crate) static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn runtime_can_open_close_and_reopen() {
        let _test_guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        struct TestHandle(u32);
        impl crate::ExcelHandleObject for TestHandle {}

        let runtime = Runtime::new();
        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish(1_u32, Vec::new());
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
        assert_eq!(runtime.enter().unwrap().state(), &1);
        let old_handles = runtime.handles().unwrap();
        let (old_token, _) = old_handles
            .prepare("old".to_owned(), || Ok(Arc::new(TestHandle(1))))
            .unwrap();

        let close_attempt = runtime.begin_final_close().unwrap();
        runtime.close_handles().unwrap();
        assert_eq!(*runtime.take_state().unwrap(), 1);
        finish_test_close(&runtime);
        drop(close_attempt);

        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish(2_u32, Vec::new());
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();
        assert_eq!(runtime.enter().unwrap().state(), &2);
        let new_handles = runtime.handles().unwrap();
        let (new_token, _) = new_handles
            .prepare("new".to_owned(), || Ok(Arc::new(TestHandle(2))))
            .unwrap();
        assert_eq!(new_handles.lookup::<TestHandle>(&new_token).unwrap().0, 2);
        assert!(matches!(
            new_handles.lookup::<TestHandle>(&old_token),
            Err(XllError::StaleHandle | XllError::InvalidHandle)
        ));
    }

    #[test]
    fn close_on_closed_runtime_invalidates_an_older_open_epoch() {
        let _test_guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let runtime = Runtime::<()>::new();
        let stale_epoch = runtime.close_epoch();

        assert!(runtime.begin_final_close().is_none());
        assert!(runtime.begin_open_if_epoch(stale_epoch).is_err());

        let mut current = runtime.begin_open().unwrap();
        runtime.publish((), Vec::new());
        runtime.finish_open(&mut current, Vec::new()).unwrap();
        assert_eq!(runtime.phase(), LifecyclePhase::Open);
    }

    #[test]
    fn a_failed_concurrent_open_cannot_rollback_the_active_attempt() {
        let _test_guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let runtime = Runtime::new();
        let mut first = runtime.begin_open().unwrap();

        assert!(runtime.begin_open().is_err());
        assert_eq!(runtime.phase(), LifecyclePhase::Opening);

        runtime.publish(11_u32, Vec::new());
        runtime.finish_open(&mut first, Vec::new()).unwrap();
        assert_eq!(runtime.phase(), LifecyclePhase::Open);
        assert_eq!(runtime.enter().unwrap().state(), &11);
        let _close = runtime.begin_final_close();
        let _ = runtime.take_state();
        finish_test_close(&runtime);
    }

    #[test]
    fn final_close_cancels_an_in_flight_open_commit() {
        let _test_guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let runtime = Arc::new(Runtime::new());
        let mut opening = runtime.begin_open().unwrap();
        runtime.publish(17_u32, Vec::new());

        let close_epoch = runtime.close_epoch();
        let closing_runtime = Arc::clone(&runtime);
        let (closed_tx, closed_rx) = mpsc::sync_channel(1);
        let closer = thread::spawn(move || {
            let _close = closing_runtime
                .begin_final_close()
                .expect("the opening runtime requires final close");
            assert_eq!(*closing_runtime.take_state().unwrap(), 17);
            finish_test_close(&closing_runtime);
            closed_tx.send(()).unwrap();
        });

        let deadline = Instant::now() + Duration::from_secs(1);
        while runtime.phase() != LifecyclePhase::Closing && Instant::now() < deadline {
            thread::yield_now();
        }
        assert_eq!(runtime.phase(), LifecyclePhase::Closing);
        assert_ne!(runtime.close_epoch(), close_epoch);
        assert!(matches!(
            runtime.finish_open(&mut opening, Vec::new()),
            Err(XllError::Closing)
        ));

        closed_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        closer.join().unwrap();
        assert_eq!(runtime.phase(), LifecyclePhase::Closed);
    }

    #[test]
    fn close_waiter_is_not_lost_when_open_rollback_finishes() {
        let _test_guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let runtime = Arc::new(Runtime::<()>::new());
        let mut opening = runtime.begin_open().unwrap();
        assert!(opening.fail());
        let rollback = runtime.acquire_open_rollback().unwrap();

        let closing_runtime = Arc::clone(&runtime);
        let (closed_tx, closed_rx) = mpsc::sync_channel(1);
        let closer = thread::spawn(move || {
            assert!(closing_runtime.begin_final_close().is_none());
            closed_tx.send(()).unwrap();
        });

        let deadline = Instant::now() + Duration::from_secs(1);
        while runtime.phase() != LifecyclePhase::Closing && Instant::now() < deadline {
            thread::yield_now();
        }
        assert_eq!(runtime.phase(), LifecyclePhase::Closing);
        finish_test_open_rollback(&runtime);
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
        let mut opening = runtime.begin_open().unwrap();
        runtime.publish((), Vec::new());
        runtime.finish_open(&mut opening, Vec::new()).unwrap();

        let first = runtime.begin_final_close().unwrap();
        drop(first);

        let second = runtime.begin_final_close().unwrap();
        let _ = runtime.take_state();
        finish_test_close(&runtime);
        drop(second);
        assert_eq!(runtime.phase(), LifecyclePhase::Closed);
    }

    #[test]
    fn close_certificate_refuses_to_publish_closed_before_state_is_released() {
        let _test_guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let runtime = Runtime::new();
        let mut opening = runtime.begin_open().unwrap();
        runtime.publish((), Vec::new());
        runtime.finish_open(&mut opening, Vec::new()).unwrap();

        let close_attempt = runtime.begin_final_close().unwrap();
        runtime.wait_for_returns();
        runtime.close_handles().unwrap();
        runtime.close_subscriptions().unwrap();
        let ingress = crate::ingress::global_ingress();
        ingress.begin_close_with(|| {
            #[cfg(any(test, feature = "shutdown-refinement"))]
            if runtime.ghost_generation_active() {
                runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::BeginClose);
            }
        });
        let exports = ingress.seal_and_drain();
        let rtd = crate::rtd::wait_for_module_quiescence().expect("RTD module quiescence");
        assert!(
            runtime
                .certify_close(ClosePrerequisites {
                    exports,
                    rtd,
                    host_callbacks: crate::shutdown::HostCallbacksDetached::new(),
                    async_stopped: crate::shutdown::AsyncStopped::new(),
                    subscriptions_stopped: crate::shutdown::SubscriptionsStopped::new(),
                    handles_quiescent: crate::shutdown::HandlesQuiescent::new(),
                    diagnostics_stopped: crate::diagnostics::DiagnosticsStopped::for_test(),
                    addin_quiesced: crate::shutdown::AddinQuiesced::new(),
                })
                .is_err()
        );
        assert_eq!(runtime.phase(), LifecyclePhase::Closing);

        assert!(runtime.take_state().is_some());
        finish_test_close(&runtime);
        drop(close_attempt);
        assert_eq!(runtime.phase(), LifecyclePhase::Closed);
    }

    #[test]
    fn close_rejects_new_calls_and_waits_for_existing_call() {
        let _test_guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let runtime = Arc::new(Runtime::new());
        let mut open_attempt = runtime.begin_open().unwrap();
        runtime.publish(7_u32, Vec::new());
        runtime.finish_open(&mut open_attempt, Vec::new()).unwrap();

        let (_export_guard, accepted) = crate::ingress::global_ingress().enter_with(|| {});
        assert!(accepted);
        let guard = runtime.enter().unwrap();
        assert!(runtime.begin_close());
        crate::ingress::global_ingress().begin_close_with(|| {});
        assert!(matches!(runtime.enter(), Err(XllError::Closing)));

        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            let _ = crate::ingress::global_ingress().seal_and_drain();
            sender.send(()).unwrap();
        });

        assert!(receiver.recv_timeout(Duration::from_millis(20)).is_err());
        drop(guard);
        drop(_export_guard);
        receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn registration_storage_is_replaceable() {
        let runtime = Runtime::<()>::new();
        runtime
            .registrations_guard()
            .push(crate::registration::PendingRegistration::from(
                RegistrationId {
                    id: 1.0,
                    excel_name: "TEST",
                },
            ));
        assert_eq!(runtime.registrations().len(), 1);
    }

    #[test]
    fn metadata_debt_storage_is_queryable() {
        let runtime = Runtime::<()>::new();
        runtime.retain_metadata_debt(vec![
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
        assert_eq!(runtime.metadata_debt().len(), 1);
        assert_eq!(runtime.metadata_debt().values().next().unwrap().len(), 2);
        assert_eq!(
            runtime.metadata_debt().values().next().unwrap()[0].expected_registration_id(),
            1.0
        );
        runtime.clear_metadata_debt_for_registrations(&[RegistrationId {
            id: 1.0,
            excel_name: "Test_Debt",
        }]);
        assert!(runtime.metadata_debt().is_empty());
    }

    #[cfg(feature = "async")]
    #[test]
    fn calculation_end_advances_the_async_task_generation() {
        let _test_guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let runtime = Runtime::<()>::new();
        runtime.start_async(1).unwrap();
        let first = runtime.calculation_id().get();
        let (first_source, first_token) = crate::cancellation::CancellationSource::new(
            crate::CancellationGuarantee::CalculationScoped,
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
                    crate::CancellationGuarantee::CalculationScoped,
                )
                .0,
            ),
            Err(XllError::ExcelValue(crate::ExcelError::NotAvailable))
        ));

        let (second_source, second_token) = crate::cancellation::CancellationSource::new(
            crate::CancellationGuarantee::CalculationScoped,
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
                crate::CancellationGuarantee::CalculationScoped,
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
