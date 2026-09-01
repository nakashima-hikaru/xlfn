//! The narrow write capability for canonical lifecycle state.
//!
//! `LifecycleControl` deliberately owns no `Runtime` reference. It can
//! mutate only the lifecycle coordinator; runtime-wide orchestration belongs
//! to `crate::runtime::orchestration`.

use crate::addin::Addin;
use crate::generation::{OpenAttemptId, OpeningGeneration, RemovalAttemptId, RemovalEpoch};
use crate::lifecycle::{
    FinalRemovalReady, HostLifecycleIntent, LifecycleAccess, LifecycleCoordinator,
    OpenFailureDisposition, OpenRollbackReady, RemovalClaim,
};
use crate::module_runtime::{ModuleAuthority, ModuleCleanupAuthority, ModuleClosing};
use crate::{XllError, XllResult};

/// Lifecycle-only write capability issued by a [`LifecycleCoordinator`].
pub(crate) struct LifecycleControl<'coordinator, A: Addin> {
    coordinator: &'coordinator LifecycleCoordinator<A>,
}

pub(crate) type PublishGenerationFailure<A> = Box<(
    XllError,
    Option<OpeningGeneration<A>>,
    Box<crate::runtime_components::GenerationServices>,
    crate::module_runtime::ModuleEpochLease,
)>;

impl<'coordinator, A: Addin> LifecycleControl<'coordinator, A> {
    pub(crate) const fn new(coordinator: &'coordinator LifecycleCoordinator<A>) -> Self {
        Self { coordinator }
    }

    pub(crate) fn access(&self) -> LifecycleAccess<'_, A> {
        self.coordinator.access()
    }

    pub(crate) fn final_removal_ready(
        &self,
        control: &LifecycleAccess<'_, A>,
        attempt: RemovalAttemptId,
    ) -> Option<FinalRemovalReady> {
        control.final_removal_ready(attempt)
    }

    pub(crate) fn open_rollback_ready(
        &self,
        control: &LifecycleAccess<'_, A>,
        attempt: RemovalAttemptId,
    ) -> Option<OpenRollbackReady> {
        control.open_rollback_ready(attempt)
    }

    pub(crate) fn clear_certified_retirement(&self, control: &mut LifecycleAccess<'_, A>) -> bool {
        self.coordinator.clear_certified_retirement(control)
    }

    pub(crate) fn begin_open_state(
        &self,
        expected_removal_epoch: RemovalEpoch,
    ) -> XllResult<OpenAttemptId> {
        let mut control = self.coordinator.access();
        if control.removal_epoch() != expected_removal_epoch.get()
            || control.phase() != crate::lifecycle::LifecyclePhase::Closed
            || control.open_attempt().is_some()
            || control.removal_attempt().is_some()
        {
            return Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::OPEN_PHASE,
            });
        }
        self.coordinator.prepare_open(&mut control);
        let attempt = self.coordinator.allocate_open_attempt(&mut control)?;
        self.coordinator.begin_opening(&mut control, attempt);
        Ok(attempt)
    }

    pub(crate) fn record_open_failure(
        &self,
        control: &mut LifecycleAccess<'_, A>,
    ) -> OpenFailureDisposition {
        self.coordinator.record_open_failure(control)
    }

    pub(crate) fn begin_removal_request(&self, control: &mut LifecycleAccess<'_, A>) {
        self.coordinator.begin_removal_request(control);
    }

    pub(crate) fn request_closing(&self, control: &mut LifecycleAccess<'_, A>) {
        self.coordinator.request_closing(control);
    }

    pub(crate) fn claim_removal(
        &self,
        control: &mut LifecycleAccess<'_, A>,
    ) -> Option<RemovalClaim> {
        self.coordinator.claim_removal(control)
    }

    pub(crate) fn wait(&self, control: &mut LifecycleAccess<'_, A>) {
        self.coordinator.wait(control);
    }

    pub(crate) fn notify_all(&self) {
        self.coordinator.notify_all();
    }

    pub(crate) fn release_removal_claim(
        &self,
        control: &mut LifecycleAccess<'_, A>,
        attempt: RemovalAttemptId,
        returned: Option<ModuleAuthority>,
    ) {
        self.coordinator
            .release_removal_claim(control, attempt, returned);
    }

    pub(crate) fn finish_final_removal(
        &self,
        control: &mut LifecycleAccess<'_, A>,
        attempt: RemovalAttemptId,
    ) -> XllResult<()> {
        self.coordinator.finish_final_removal(control, attempt)
    }

    pub(crate) fn finish_open_rollback(
        &self,
        control: &mut LifecycleAccess<'_, A>,
        attempt: RemovalAttemptId,
    ) -> XllResult<()> {
        self.coordinator.finish_open_rollback(control, attempt)
    }

    pub(crate) fn complete_open_abort(&self, closing: ModuleClosing) {
        self.coordinator.complete_open_abort(closing);
    }

    pub(crate) fn stage_opening_generation(
        &self,
        attempt_id: OpenAttemptId,
        opening: OpeningGeneration<A>,
    ) -> Result<(), (XllError, OpeningGeneration<A>)> {
        let mut control = self.coordinator.access();
        if control.open_attempt() != Some(attempt_id)
            || control.phase() != crate::lifecycle::LifecyclePhase::Opening
        {
            return Err((XllError::Closing, opening));
        }
        self.coordinator
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
        services: Box<crate::runtime_components::GenerationServices>,
        module_epoch: crate::module_runtime::ModuleEpochLease,
    ) -> Result<(), PublishGenerationFailure<A>> {
        self.coordinator
            .publish_opening_generation_locked(control, generation, services, module_epoch)
            .map_err(|failure| {
                Box::new((
                    failure.error,
                    failure.opening,
                    failure.services,
                    failure.module_epoch,
                ))
            })
    }

    pub(crate) fn finish_open_state(
        &self,
        control: &mut LifecycleAccess<'_, A>,
        generation: crate::generation::RuntimeGeneration,
    ) -> XllResult<()> {
        self.coordinator.commit_open(control, generation)
    }

    pub(crate) fn complete_open_abort_locked(
        &self,
        control: &mut LifecycleAccess<'_, A>,
        closing: ModuleClosing,
    ) {
        self.coordinator
            .complete_open_abort_locked(control, closing);
    }

    pub(crate) fn request_explicit_removal(&self) {
        self.coordinator
            .set_host_intent(HostLifecycleIntent::ExplicitRemovalRequested);
    }

    pub(crate) fn complete_explicit_removal(&self) {
        self.coordinator
            .set_host_intent(HostLifecycleIntent::ExplicitRemovalComplete);
    }

    pub(crate) fn clear_host_intent(&self) {
        self.coordinator.set_host_intent(HostLifecycleIntent::None);
    }

    pub(crate) fn quarantine_state(&self, control: &mut LifecycleAccess<'_, A>) {
        self.coordinator.quarantine_core(control);
    }

    #[cfg(test)]
    pub(crate) fn take_module_closing_for_test(
        &self,
        control: &mut LifecycleAccess<'_, A>,
    ) -> Option<ModuleClosing> {
        self.coordinator.take_module_closing_for_close(control)
    }

    pub(crate) fn take_module_cleanup_authority(&self) -> Option<ModuleCleanupAuthority> {
        self.coordinator.take_module_cleanup_for_quarantine()
    }

    pub(crate) fn take_opening_for_rollback(&self) -> Option<OpeningGeneration<A>> {
        self.coordinator.take_opening_for_rollback()
    }
}
