//! Runtime-wide lifecycle orchestration.
//!
//! This module is the composition layer between the canonical lifecycle state
//! machine and the return, module, refinement, and quarantine components. The
//! lifecycle control capability itself is intentionally narrower and does not
//! own a `Runtime` reference.

use crate::addin::Addin;
use crate::generation::{
    ExecutionGeneration, OpenAttemptId, OpeningGeneration, RemovalEpoch, RuntimeGeneration,
};
use crate::lifecycle::{LifecycleControl, LifecyclePhase, OpenFailureDisposition};
use crate::runtime::Runtime;
use crate::runtime::shutdown::RemovalOwner;
use crate::runtime_components::QuarantineReason;
use crate::runtime_open_txn::{OpenAttemptBegun, OpeningTxn};
use crate::{XllError, XllResult};
use std::sync::Arc;

/// Coordinates transitions that span the lifecycle state machine and runtime
/// resources. It is intentionally the only object in this module that holds a
/// `Runtime` reference.
pub(crate) struct RuntimeOrchestrator<'runtime, A: Addin> {
    runtime: &'runtime Runtime<A>,
}

impl<'runtime, A: Addin> RuntimeOrchestrator<'runtime, A> {
    pub(crate) const fn new(runtime: &'runtime Runtime<A>) -> Self {
        Self { runtime }
    }

    fn lifecycle(&self) -> LifecycleControl<'_, A> {
        LifecycleControl::new(&self.runtime.lifecycle)
    }

    pub(crate) fn begin_open_if_epoch(
        &self,
        expected_removal_epoch: RemovalEpoch,
    ) -> XllResult<OpeningTxn<'runtime, A, OpenAttemptBegun>> {
        #[cfg(test)]
        let test_module_lease = crate::ingress::acquire_test_module_lease();

        let lifecycle = self.lifecycle();
        let attempt_id = lifecycle.begin_open_state(expected_removal_epoch)?;
        self.runtime.return_protocol.reopen_admission()?;

        let module_opening = crate::module_runtime::begin_open();
        #[cfg(test)]
        {
            *self.runtime.lifecycle.test_module_lease.lock() = Some(test_module_lease);
        }
        self.runtime
            .refinement
            .begin_open(self.runtime, expected_removal_epoch.get(), attempt_id);
        Ok(OpeningTxn::new_begun(
            self.runtime,
            attempt_id,
            module_opening,
        ))
    }

    #[cfg(test)]
    pub(crate) fn begin_open(&self) -> XllResult<OpeningTxn<'runtime, A, OpenAttemptBegun>> {
        self.begin_open_if_epoch(self.runtime.removal_epoch())
    }

    pub(crate) fn mark_open_failed(&self, attempt_id: OpenAttemptId) -> OpenFailureDisposition {
        let lifecycle = self.lifecycle();
        let mut control = lifecycle.access();
        if control.open_attempt() != Some(attempt_id) {
            return OpenFailureDisposition::ClosingOwnsCleanup;
        }

        if control.phase() == LifecyclePhase::Opening {
            self.runtime.return_protocol.close_admission();
        }
        let disposition = lifecycle.record_open_failure(&mut control);
        lifecycle.notify_all();
        drop(control);
        self.runtime.refinement.fail_open(self.runtime, attempt_id);
        disposition
    }

    pub(crate) fn quarantine(&self) {
        let lifecycle = self.lifecycle();
        let mut control = lifecycle.access();
        self.runtime.return_protocol.close_admission();
        lifecycle.quarantine_state(&mut control);
    }

    pub(crate) fn quarantine_shared_state(
        &self,
        generation: Option<RuntimeGeneration>,
        shared_state: A::SharedState,
        reason: QuarantineReason,
    ) {
        self.runtime
            .quarantine
            .retain_shared_state(generation, shared_state, reason);
    }

    pub(crate) fn quarantine_layers(
        &self,
        generation: Option<RuntimeGeneration>,
        layers: A::Layers,
        reason: QuarantineReason,
    ) {
        self.runtime
            .quarantine
            .retain_layers(generation, layers, reason);
    }

    pub(crate) fn quarantine_generation(
        &self,
        generation: Option<RuntimeGeneration>,
        root: ExecutionGeneration<A>,
        reason: QuarantineReason,
    ) {
        self.runtime
            .quarantine
            .retain_generation(generation, root, reason);
    }

    pub(crate) fn quarantine_shared_generation(
        &self,
        generation: Option<RuntimeGeneration>,
        root: Arc<ExecutionGeneration<A>>,
        reason: QuarantineReason,
    ) {
        self.runtime
            .quarantine
            .retain_shared_generation(generation, root, reason);
    }

    pub(crate) fn quarantine_opening_generation(
        &self,
        generation: Option<RuntimeGeneration>,
        opening: OpeningGeneration<A>,
        reason: QuarantineReason,
    ) {
        let OpeningGeneration {
            shared_state,
            layers,
            init_config: _,
        } = opening;
        if let Some(id) = generation {
            self.runtime.quarantine.retain_generation(
                Some(id),
                ExecutionGeneration {
                    id,
                    shared_state,
                    layers,
                },
                reason,
            );
        } else {
            self.runtime
                .quarantine
                .retain_shared_state(None, shared_state, reason);
            self.runtime.quarantine.retain_layers(None, layers, reason);
        }
    }

    #[cfg(test)]
    pub(crate) fn begin_close(&self) -> bool {
        let lifecycle = self.lifecycle();
        let mut control = lifecycle.access();
        if matches!(
            control.phase(),
            LifecyclePhase::Opening | LifecyclePhase::Open
        ) {
            self.runtime.return_protocol.close_admission();
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

    pub(crate) fn begin_final_removal(&self) -> Option<RemovalOwner<'runtime, A>> {
        let lifecycle = self.lifecycle();
        let mut wait_guard = lifecycle.access();
        lifecycle.begin_removal_request(&mut wait_guard);
        self.runtime.return_protocol.close_admission();
        let mut request_recorded = false;

        'retry: loop {
            let decision = 'decision: {
                match wait_guard.phase() {
                    LifecyclePhase::Closed => {
                        if wait_guard.removal_attempt().is_none()
                            && self.runtime.returns_are_quiescent()
                        {
                            self.runtime
                                .refinement
                                .request_final_close(self.runtime, &mut request_recorded);
                            break 'decision Some(None);
                        }
                        if wait_guard.removal_attempt().is_none() {
                            drop(wait_guard);
                            self.runtime.return_protocol.wait_for_returns();
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
                    self.runtime
                        .refinement
                        .request_final_close(self.runtime, &mut request_recorded);
                }

                if wait_guard.phase() != LifecyclePhase::Closed
                    && wait_guard.open_attempt().is_none()
                    && let Some(claim) = lifecycle.claim_removal(&mut wait_guard)
                {
                    self.runtime
                        .refinement
                        .acquire_final_close_owner(self.runtime);
                    Some(Some(claim))
                } else {
                    None
                }
            };

            match decision {
                Some(Some(claim)) => return Some(RemovalOwner::new(self.runtime, claim)),
                Some(None) => return None,
                None => lifecycle.wait(&mut wait_guard),
            }
        }
    }

    pub(crate) fn take_opening_for_rollback(&self) -> Option<OpeningGeneration<A>> {
        self.lifecycle().take_opening_for_rollback()
    }

    pub(crate) fn acquire_open_rollback(&self) -> Option<RemovalOwner<'runtime, A>> {
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
                self.runtime
                    .refinement
                    .acquire_open_rollback_owner(self.runtime);
                return Some(RemovalOwner::new(self.runtime, claim));
            }
            lifecycle.wait(&mut wait_guard);
        }
    }
}
