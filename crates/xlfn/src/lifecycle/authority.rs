use crate::addin::Addin;
use crate::generation::{ExecutionGeneration, OpenAttemptId, OpeningGeneration, RemovalEpoch};
use crate::lifecycle::{
    HostLifecycleIntent, LifecycleAccess, OpenAttemptBegun, OpeningTxn, RemovalOwner,
};
use crate::runtime::Runtime;
use crate::runtime_components::{GenerationServices, QuarantineReason};
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

impl<'runtime, A: Addin> LifecycleAuthority<'runtime, A> {
    pub(crate) const fn new(runtime: &'runtime Runtime<A>) -> Self {
        Self { runtime }
    }

    pub(crate) fn install_module_closing(&self, closing: crate::module_runtime::ModuleClosing) {
        self.runtime.lifecycle.install_module_closing(closing);
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

    pub(crate) fn publish_opening_generation(
        &self,
        control: &mut LifecycleAccess<'_, A>,
        attempt_id: OpenAttemptId,
        module_epoch: crate::module_runtime::ModuleEpochLease,
    ) -> XllResult<()> {
        let generation = attempt_id.into_runtime_generation();
        let config = control.opening_config().ok_or(XllError::Internal {
            diagnostic_id: crate::diagnostics::id::DiagnosticId::OPEN_STATE,
        })?;
        let armed_services = GenerationServices::arm_generation(
            generation,
            config,
            crate::rtd::RtdSubscriptionHost::production(crate::module_runtime::ingress()),
        )?;
        let services = armed_services.commit();
        if let Err(failure) = self.runtime.lifecycle.publish_opening_generation_locked(
            control,
            generation,
            Arc::clone(&services),
            module_epoch,
        ) {
            services.disarm_or_abort();
            if let Some(opening) = failure.opening {
                self.quarantine_opening_generation(
                    Some(generation),
                    opening,
                    QuarantineReason::OpenStateInvariant,
                );
            }
            return Err(failure.error);
        }
        Ok(())
    }

    pub(crate) fn commit_open(
        &self,
        attempt_id: OpenAttemptId,
        module_opening: crate::module_runtime::ModuleOpening,
        journal: &mut crate::registration::HostMutationJournal,
    ) -> XllResult<()> {
        let mut control = self.runtime.lifecycle.access();
        if control.open_attempt() != Some(attempt_id) {
            return Err(XllError::Closing);
        }

        let registration_ids = journal
            .pending_registrations
            .iter()
            .map(|entry| entry.registration)
            .collect::<Vec<_>>();
        self.runtime
            .clear_metadata_debt_for_registrations(&registration_ids);
        self.runtime.host.merge(std::mem::take(journal));
        let can_commit = control.phase() == crate::lifecycle::LifecyclePhase::Opening;
        if can_commit {
            let ingress = crate::module_runtime::ingress();
            ingress
                .complete_open(|| {
                    self.publish_opening_generation(
                        &mut control,
                        attempt_id,
                        module_opening.commit(),
                    )?;
                    let generation = attempt_id.into_runtime_generation();
                    self.runtime
                        .refinement
                        .commit_open(self.runtime, attempt_id, || {
                            self.runtime
                                .lifecycle
                                .commit_open(&mut control, generation)?;
                            if control.phase() != crate::lifecycle::LifecyclePhase::Open
                                || control.last_committed_generation() != Some(generation)
                                || control.open_attempt().is_some()
                            {
                                crate::lifecycle::fail_stop_invariant(
                                    "xlAutoOpen commit postcondition",
                                    &XllError::Internal {
                                        diagnostic_id:
                                            crate::diagnostics::id::DiagnosticId::OPEN_STATE,
                                    },
                                );
                            }
                            Ok(())
                        })?;
                    Ok::<(), XllError>(())
                })
                .unwrap_or_else(|_| opening_publication_lost())?;
            self.runtime.lifecycle.notify_all();
            Ok(())
        } else {
            self.reject_open_attempt(&mut control, module_opening);
            self.runtime.lifecycle.notify_all();
            drop(control);
            self.runtime
                .refinement
                .reject_open(self.runtime, attempt_id);
            Err(XllError::Closing)
        }
    }

    fn reject_open_attempt(
        &self,
        control: &mut LifecycleAccess<'_, A>,
        module_opening: crate::module_runtime::ModuleOpening,
    ) {
        self.runtime.lifecycle.reject_open_attempt(control);
        self.runtime
            .lifecycle
            .install_module_closing_locked(control, module_opening.rollback(|| {}));
    }

    pub(crate) fn fail_open(
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
            .unwrap_or_else(crate::module_runtime::begin_close_for_test)
    }

    #[cfg(test)]
    pub(crate) fn begin_close(&self) -> bool {
        let mut control = self.runtime.lifecycle.access();
        let should_close = crate::module_runtime::ingress().with_linearization(|| {
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
        });
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
            let decision = crate::module_runtime::ingress().with_linearization(|| {
                match wait_guard.phase() {
                    crate::lifecycle::LifecyclePhase::Closed => {
                        if wait_guard.removal_attempt().is_none()
                            && self.runtime.returns_are_quiescent()
                        {
                            self.runtime
                                .refinement
                                .request_final_close(self.runtime, &mut request_recorded);
                            return Some(None);
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
                    crate::lifecycle::LifecyclePhase::Quarantined => return Some(None),
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
            });
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

    pub(crate) fn take_module_closing_for_quarantine(
        &self,
    ) -> crate::module_runtime::ModuleClosing {
        self.runtime
            .lifecycle
            .take_module_closing_for_quarantine()
            .unwrap_or_else(|| crate::module_runtime::begin_close_for_quarantine(|| {}))
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

#[cold]
fn opening_publication_lost() -> ! {
    #[cfg(not(test))]
    {
        tracing::error!("lifecycle opening publication lost its ingress linearization");
        std::process::abort();
    }
    #[cfg(test)]
    panic!("lifecycle opening publication lost its ingress linearization");
}
