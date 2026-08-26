use crate::addin::Addin;
use crate::generation::{ExecutionGeneration, OpenAttemptId, OpeningGeneration, RemovalEpoch};
use crate::lifecycle::{
    HostLifecycleIntent, LifecycleAccess, OpenAttemptBegun, OpeningTxn, RemovalOwner,
};
use crate::runtime::Runtime;
use crate::runtime_components::QuarantineReason;
use crate::{XllError, XllResult};
use std::sync::Arc;

/// Lifecycle write capability issued by a [`Runtime`] aggregate.
///
/// Call-facing code has no access to this capability's backend operations;
/// lifecycle boundaries use it to request open, rollback, removal, and
/// quarantine transitions. The underlying runtime remains the single
/// ownership root, while this type is the lifecycle domain's authority.
pub(crate) struct LifecycleAuthority<'runtime, A: Addin> {
    runtime: &'runtime Runtime<A>,
}

pub(crate) type PublishGenerationFailure<A> = Box<(
    XllError,
    Option<OpeningGeneration<A>>,
    crate::module_runtime::ModuleEpochLease,
)>;

impl<'runtime, A: Addin> LifecycleAuthority<'runtime, A> {
    pub(crate) const fn new(runtime: &'runtime Runtime<A>) -> Self {
        Self { runtime }
    }

    pub(crate) fn install_module_closing(&self, closing: crate::module_runtime::ModuleClosing) {
        self.runtime.lifecycle.install_module_closing(closing);
    }

    pub(crate) fn install_module_cleanup_authority(
        &self,
        authority: crate::module_runtime::ModuleCleanupAuthority,
    ) {
        self.runtime.lifecycle.install_module_cleanup(authority);
    }

    pub(crate) fn begin_open_if_epoch(
        &self,
        expected_removal_epoch: RemovalEpoch,
    ) -> XllResult<OpeningTxn<'runtime, A, OpenAttemptBegun>> {
        #[cfg(test)]
        let test_module_lease = crate::ingress::acquire_test_module_lease();
        let mut control = self.runtime.lifecycle.access();
        if control.removal_epoch() != expected_removal_epoch.get()
            || control.phase() != crate::lifecycle::LifecyclePhase::Closed
            || control.open_attempt().is_some()
            || control.removal_attempt().is_some()
        {
            return Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::OPEN_PHASE,
            });
        }

        self.runtime.lifecycle.prepare_open(&mut control);
        let attempt_id = self.runtime.lifecycle.allocate_open_attempt(&mut control)?;
        self.runtime.return_protocol.reopen_admission()?;

        let module_opening = crate::module_runtime::begin_open();
        #[cfg(test)]
        {
            *self.runtime.lifecycle.test_module_lease.lock() = Some(test_module_lease);
        }
        self.runtime
            .lifecycle
            .begin_opening(&mut control, attempt_id);
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

    pub(crate) fn stage_opening_generation(
        &self,
        attempt_id: OpenAttemptId,
        opening: OpeningGeneration<A>,
    ) -> Result<(), (XllError, OpeningGeneration<A>)> {
        let mut control = self.runtime.lifecycle.access();
        if control.open_attempt() != Some(attempt_id)
            || control.phase() != crate::lifecycle::LifecyclePhase::Opening
        {
            return Err((XllError::Closing, opening));
        }
        self.runtime
            .lifecycle
            .stage_opening_generation_locked(&mut control, opening)
    }

    pub(crate) fn validate_open_attempt(
        &self,
        control: &LifecycleAccess<'_, A>,
        attempt_id: OpenAttemptId,
    ) -> XllResult<()> {
        if control.open_attempt() == Some(attempt_id) {
            Ok(())
        } else {
            Err(XllError::Closing)
        }
    }

    pub(crate) fn publish_generation_state(
        &self,
        control: &mut LifecycleAccess<'_, A>,
        generation: crate::generation::RuntimeGeneration,
        services: Arc<crate::runtime_components::GenerationServices>,
        module_epoch: crate::module_runtime::ModuleEpochLease,
    ) -> Result<(), PublishGenerationFailure<A>> {
        self.runtime
            .lifecycle
            .publish_opening_generation_locked(control, generation, services, module_epoch)
            .map_err(|failure| Box::new((failure.error, failure.opening, failure.module_epoch)))
    }

    pub(crate) fn finish_open_state(
        &self,
        control: &mut LifecycleAccess<'_, A>,
        generation: crate::generation::RuntimeGeneration,
    ) -> XllResult<()> {
        self.runtime.lifecycle.commit_open(control, generation)
    }

    pub(crate) fn reject_open_state(&self, control: &mut LifecycleAccess<'_, A>) {
        self.runtime.lifecycle.reject_open_attempt(control);
    }

    pub(crate) fn install_module_closing_locked(
        &self,
        control: &mut LifecycleAccess<'_, A>,
        closing: crate::module_runtime::ModuleClosing,
    ) {
        self.runtime
            .lifecycle
            .install_module_closing_locked(control, closing);
    }

    pub(crate) fn mark_open_failed(
        &self,
        attempt_id: OpenAttemptId,
    ) -> crate::lifecycle::OpenFailureDisposition {
        let mut control = self.runtime.lifecycle.access();
        if control.open_attempt() != Some(attempt_id) {
            return crate::lifecycle::OpenFailureDisposition::ClosingOwnsCleanup;
        }

        if control.phase() == crate::lifecycle::LifecyclePhase::Opening {
            self.runtime.return_protocol.close_admission();
        }
        let disposition = self.runtime.lifecycle.record_open_failure(&mut control);
        self.runtime.lifecycle.notify_all();
        drop(control);
        self.runtime.refinement.fail_open(self.runtime, attempt_id);
        disposition
    }

    pub(crate) fn request_explicit_removal(&self) {
        self.runtime
            .lifecycle
            .set_host_intent(HostLifecycleIntent::ExplicitRemovalRequested);
    }

    pub(crate) fn complete_explicit_removal(&self) {
        self.runtime
            .lifecycle
            .set_host_intent(HostLifecycleIntent::ExplicitRemovalComplete);
    }

    pub(crate) fn clear_host_intent(&self) {
        self.runtime
            .lifecycle
            .set_host_intent(HostLifecycleIntent::None);
    }

    pub(crate) fn quarantine(&self) {
        let mut control = self.runtime.lifecycle.access();
        self.runtime.return_protocol.close_admission();
        self.runtime.lifecycle.quarantine_core(&mut control);
    }

    pub(crate) fn quarantine_shared_state(
        &self,
        generation: Option<crate::generation::RuntimeGeneration>,
        shared_state: A::SharedState,
        reason: QuarantineReason,
    ) {
        self.runtime
            .quarantine
            .retain_shared_state(generation, shared_state, reason);
    }

    pub(crate) fn quarantine_layers(
        &self,
        generation: Option<crate::generation::RuntimeGeneration>,
        layers: A::Layers,
        reason: QuarantineReason,
    ) {
        self.runtime
            .quarantine
            .retain_layers(generation, layers, reason);
    }

    pub(crate) fn quarantine_generation(
        &self,
        generation: Option<crate::generation::RuntimeGeneration>,
        root: ExecutionGeneration<A>,
        reason: QuarantineReason,
    ) {
        self.runtime
            .quarantine
            .retain_generation(generation, root, reason);
    }

    pub(crate) fn quarantine_shared_generation(
        &self,
        generation: Option<crate::generation::RuntimeGeneration>,
        root: Arc<ExecutionGeneration<A>>,
        reason: QuarantineReason,
    ) {
        self.runtime
            .quarantine
            .retain_shared_generation(generation, root, reason);
    }

    pub(crate) fn quarantine_opening_generation(
        &self,
        generation: Option<crate::generation::RuntimeGeneration>,
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

    pub(crate) fn take_module_closing_for_owner(
        &self,
        control: &mut LifecycleAccess<'_, A>,
    ) -> crate::module_runtime::ModuleClosing {
        self.runtime
            .lifecycle
            .take_module_closing_for_close(control)
            .unwrap_or_else(|| {
                crate::lifecycle::fail_stop_invariant(
                    "removal owner lacks module close authority",
                    &XllError::Internal {
                        diagnostic_id: crate::diagnostics::id::DiagnosticId::CLOSE_RUNTIME,
                    },
                )
            })
    }

    #[cfg(test)]
    pub(crate) fn take_module_closing_for_test(
        &self,
        control: &mut LifecycleAccess<'_, A>,
    ) -> crate::module_runtime::ModuleClosing {
        self.runtime
            .lifecycle
            .take_module_closing_for_close(control)
            .unwrap_or_else(|| {
                crate::lifecycle::fail_stop_invariant(
                    "test removal owner lacks module close authority",
                    &XllError::Internal {
                        diagnostic_id: crate::diagnostics::id::DiagnosticId::CLOSE_RUNTIME,
                    },
                )
            })
    }

    #[cfg(test)]
    pub(crate) fn begin_close(&self) -> bool {
        let mut control = self.runtime.lifecycle.access();
        let should_close = {
            if matches!(
                control.phase(),
                crate::lifecycle::LifecyclePhase::Opening | crate::lifecycle::LifecyclePhase::Open
            ) {
                self.runtime.return_protocol.close_admission();
                self.runtime.lifecycle.request_closing(&mut control);
                true
            } else {
                false
            }
        };
        if should_close {
            let _ = self.take_module_closing_for_test(&mut control);
        }
        should_close
    }

    pub(crate) fn begin_final_removal(&self) -> Option<RemovalOwner<'runtime, A>> {
        let mut wait_guard = self.runtime.lifecycle.access();
        self.runtime
            .lifecycle
            .begin_removal_request(&mut wait_guard);
        self.runtime.return_protocol.close_admission();
        let mut request_recorded = false;
        loop {
            let decision = 'decision: {
                match wait_guard.phase() {
                    crate::lifecycle::LifecyclePhase::Closed => {
                        if wait_guard.removal_attempt().is_none()
                            && self.runtime.returns_are_quiescent()
                        {
                            self.runtime
                                .refinement
                                .request_final_close(self.runtime, &mut request_recorded);
                            break 'decision Some(None);
                        }
                        if wait_guard.removal_attempt().is_none() {
                            self.runtime.lifecycle.request_closing(&mut wait_guard);
                        }
                    }
                    crate::lifecycle::LifecyclePhase::Closing => {}
                    crate::lifecycle::LifecyclePhase::Opening
                    | crate::lifecycle::LifecyclePhase::Open
                    | crate::lifecycle::LifecyclePhase::OpenRollbackPending => {
                        self.runtime.lifecycle.request_closing(&mut wait_guard);
                    }
                    crate::lifecycle::LifecyclePhase::Quarantined => break 'decision Some(None),
                }

                if !request_recorded {
                    if !matches!(
                        wait_guard.phase(),
                        crate::lifecycle::LifecyclePhase::Closed
                            | crate::lifecycle::LifecyclePhase::Closing
                    ) {
                        crate::lifecycle::fail_stop_invariant(
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

                if wait_guard.phase() != crate::lifecycle::LifecyclePhase::Closed
                    && wait_guard.open_attempt().is_none()
                    && let Some(attempt) =
                        self.runtime.lifecycle.claim_removal_owner(&mut wait_guard)
                {
                    self.runtime
                        .refinement
                        .acquire_final_close_owner(self.runtime);
                    Some(Some(attempt))
                } else {
                    None
                }
            };
            match decision {
                Some(Some(attempt)) => {
                    let module_closing = self.take_module_closing_for_owner(&mut wait_guard);
                    return Some(RemovalOwner::new(self.runtime, attempt, module_closing));
                }
                Some(None) => return None,
                None => self.runtime.lifecycle.wait(&mut wait_guard),
            }
        }
    }

    pub(crate) fn take_module_cleanup_authority_for_quarantine(
        &self,
    ) -> Option<crate::module_runtime::ModuleCleanupAuthority> {
        let authority = self.runtime.lifecycle.take_module_cleanup_for_quarantine();
        if authority.is_none()
            && (self.runtime.lifecycle.access().module_epoch_id().is_some()
                || crate::module_runtime::ingress().phase() != crate::ingress::PHASE_CLOSED)
        {
            crate::lifecycle::fail_stop_invariant(
                "active module epoch lacks affine close authority",
                &XllError::Internal {
                    diagnostic_id: crate::diagnostics::id::DiagnosticId::CLOSE_RUNTIME,
                },
            );
        }
        authority
    }

    pub(crate) fn take_opening_for_rollback(&self) -> Option<OpeningGeneration<A>> {
        self.runtime.lifecycle.take_opening_for_rollback()
    }

    pub(crate) fn acquire_open_rollback(&self) -> Option<RemovalOwner<'runtime, A>> {
        let mut wait_guard = self.runtime.lifecycle.access();
        loop {
            match wait_guard.phase() {
                crate::lifecycle::LifecyclePhase::Closed => return None,
                crate::lifecycle::LifecyclePhase::OpenRollbackPending => {}
                crate::lifecycle::LifecyclePhase::Closing
                | crate::lifecycle::LifecyclePhase::Opening
                | crate::lifecycle::LifecyclePhase::Open
                | crate::lifecycle::LifecyclePhase::Quarantined => return None,
            }
            if let Some(attempt) = self.runtime.lifecycle.claim_removal_owner(&mut wait_guard) {
                self.runtime
                    .refinement
                    .acquire_open_rollback_owner(self.runtime);
                let module_closing = self.take_module_closing_for_owner(&mut wait_guard);
                return Some(RemovalOwner::new(self.runtime, attempt, module_closing));
            }
            self.runtime.lifecycle.wait(&mut wait_guard);
        }
    }
}
