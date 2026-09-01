//! Runtime-wide lifecycle orchestration.
//!
//! This module coordinates lifecycle-owned transitions with the small set of
//! runtime facilities required to issue an operation claim. It deliberately
//! does not own a `Runtime` reference; composition-root wiring happens in
//! `Runtime` itself.

use crate::addin::Addin;
use crate::generation::{
    ExecutionGeneration, OpenAttemptId, OpeningGeneration, RemovalEpoch, RuntimeGeneration,
};
use crate::lifecycle::{LifecycleControl, LifecyclePhase, OpenFailureDisposition};
use crate::runtime::capabilities::OpenDeps;
use crate::runtime_components::QuarantineReason;
use crate::{XllError, XllResult};

/// The result of beginning an open attempt. The composition root turns this
/// into an `OpeningTxn` by attaching the operation-scoped open capability.
pub(crate) struct OpenAttemptStart {
    pub(crate) attempt: OpenAttemptId,
    pub(crate) module_opening: crate::module_runtime::ModuleOpening,
}

/// Lifecycle orchestration without ambient access to the runtime aggregate.
pub(crate) struct LifecycleOrchestrator<'a, A: Addin> {
    lifecycle: &'a crate::lifecycle::LifecycleCoordinator<A>,
    returns: &'a crate::runtime_components::ReturnProtocol,
    quarantine: &'a crate::runtime_components::QuarantineVault<A>,
    observer: &'a crate::runtime::observer::RuntimeObserver,
}

impl<'a, A: Addin> LifecycleOrchestrator<'a, A> {
    pub(in crate::runtime) fn new(deps: OpenDeps<'a, A>) -> Self {
        Self {
            lifecycle: deps.lifecycle(),
            returns: deps.returns(),
            quarantine: deps.quarantine(),
            observer: deps.observer(),
        }
    }

    fn lifecycle(&self) -> LifecycleControl<'_, A> {
        LifecycleControl::new(self.lifecycle)
    }

    pub(crate) fn begin_open_if_epoch(
        &self,
        expected_removal_epoch: RemovalEpoch,
    ) -> XllResult<OpenAttemptStart> {
        #[cfg(test)]
        let test_module_lease = crate::ingress::acquire_test_module_lease();

        let lifecycle = self.lifecycle();
        let attempt_id = lifecycle.begin_open_state(expected_removal_epoch)?;
        self.returns.reopen_admission()?;

        let module_opening = crate::module_runtime::begin_open();
        #[cfg(test)]
        {
            *self.lifecycle.test_module_lease.lock() = Some(test_module_lease);
        }
        self.observer
            .begin_open(self.returns, expected_removal_epoch.get(), attempt_id);
        Ok(OpenAttemptStart {
            attempt: attempt_id,
            module_opening,
        })
    }

    pub(crate) fn mark_open_failed(&self, attempt_id: OpenAttemptId) -> OpenFailureDisposition {
        let lifecycle = self.lifecycle();
        let mut control = lifecycle.access();
        if control.open_attempt() != Some(attempt_id) {
            return OpenFailureDisposition::ClosingOwnsCleanup;
        }

        if control.phase() == LifecyclePhase::Opening {
            self.returns.close_admission();
        }
        let disposition = lifecycle.record_open_failure(&mut control);
        lifecycle.notify_all();
        drop(control);
        self.observer.fail_open(attempt_id);
        disposition
    }

    pub(crate) fn quarantine(&self) {
        let lifecycle = self.lifecycle();
        let mut control = lifecycle.access();
        self.returns.close_admission();
        lifecycle.quarantine_state(&mut control);
    }

    pub(crate) fn quarantine_shared_state(
        &self,
        generation: Option<RuntimeGeneration>,
        shared_state: A::SharedState,
        reason: QuarantineReason,
    ) {
        self.quarantine
            .retain_shared_state(generation, shared_state, reason);
    }

    pub(crate) fn quarantine_layers(
        &self,
        generation: Option<RuntimeGeneration>,
        layers: A::Layers,
        reason: QuarantineReason,
    ) {
        self.quarantine.retain_layers(generation, layers, reason);
    }

    pub(crate) fn quarantine_generation(
        &self,
        generation: Option<RuntimeGeneration>,
        root: ExecutionGeneration<A>,
        reason: QuarantineReason,
    ) {
        self.quarantine.retain_generation(generation, root, reason);
    }

    #[cfg(test)]
    pub(crate) fn begin_close(&self) -> bool {
        let lifecycle = self.lifecycle();
        let mut control = lifecycle.access();
        if matches!(
            control.phase(),
            LifecyclePhase::Opening | LifecyclePhase::Open
        ) {
            self.returns.close_admission();
            lifecycle.request_closing(&mut control);
            let _ = lifecycle
                .take_module_closing_for_test(&mut control)
                .unwrap_or_else(|| {
                    crate::boundary::fail_stop_invariant(
                        "test removal owner lacks module close authority",
                        &XllError::Internal {
                            diagnostic_id: crate::diagnostics::id::DiagnosticId::CLOSE_RUNTIME,
                        },
                    )
                });
            true
        } else {
            false
        }
    }

    pub(crate) fn begin_final_removal(&self) -> Option<crate::lifecycle::RemovalClaim> {
        let lifecycle = self.lifecycle();
        let mut wait_guard = lifecycle.access();
        lifecycle.begin_removal_request(&mut wait_guard);
        self.returns.close_admission();
        let mut request_recorded = false;

        'retry: loop {
            let decision = 'decision: {
                match wait_guard.phase() {
                    LifecyclePhase::Closed => {
                        if wait_guard.removal_attempt().is_none()
                            && self.returns.returns_are_quiescent()
                        {
                            self.observer.request_final_close(&mut request_recorded);
                            break 'decision Some(None);
                        }
                        if wait_guard.removal_attempt().is_none() {
                            drop(wait_guard);
                            self.returns.wait_for_returns();
                            wait_guard = lifecycle.access();
                            continue 'retry;
                        }
                    }
                    LifecyclePhase::Closing => {}
                    LifecyclePhase::Opening
                    | LifecyclePhase::Open
                    | LifecyclePhase::OpenRollbackPending => {
                        lifecycle.request_closing(&mut wait_guard);
                    }
                    LifecyclePhase::Quarantined => break 'decision Some(None),
                }

                if !request_recorded {
                    if !matches!(
                        wait_guard.phase(),
                        LifecyclePhase::Closed | LifecyclePhase::Closing
                    ) {
                        crate::boundary::fail_stop_invariant(
                            "xlAutoRemove close-request postcondition",
                            &XllError::Internal {
                                diagnostic_id: crate::diagnostics::id::DiagnosticId::CLOSE_WAIT,
                            },
                        );
                    }
                    self.observer.request_final_close(&mut request_recorded);
                }

                if wait_guard.phase() != LifecyclePhase::Closed
                    && wait_guard.open_attempt().is_none()
                    && let Some(claim) = lifecycle.claim_removal(&mut wait_guard)
                {
                    self.observer.acquire_final_close_owner();
                    Some(Some(claim))
                } else {
                    None
                }
            };

            match decision {
                Some(Some(claim)) => return Some(claim),
                Some(None) => return None,
                None => lifecycle.wait(&mut wait_guard),
            }
        }
    }

    pub(crate) fn take_opening_for_rollback(&self) -> Option<OpeningGeneration<A>> {
        self.lifecycle().take_opening_for_rollback()
    }

    pub(crate) fn acquire_open_rollback(&self) -> Option<crate::lifecycle::RemovalClaim> {
        let lifecycle = self.lifecycle();
        let mut wait_guard = lifecycle.access();
        loop {
            match wait_guard.phase() {
                LifecyclePhase::Closed => return None,
                LifecyclePhase::OpenRollbackPending => {}
                LifecyclePhase::Closing
                | LifecyclePhase::Opening
                | LifecyclePhase::Open
                | LifecyclePhase::Quarantined => return None,
            }
            if let Some(claim) = lifecycle.claim_removal(&mut wait_guard) {
                self.observer.acquire_open_rollback_owner();
                return Some(claim);
            }
            lifecycle.wait(&mut wait_guard);
        }
    }
}
