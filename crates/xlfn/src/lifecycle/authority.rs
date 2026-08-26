//! The narrow write capability for canonical lifecycle state.
//!
//! `LifecycleControl` deliberately owns no `Runtime` reference. It can
//! mutate only the lifecycle coordinator; runtime-wide orchestration belongs
//! to `crate::runtime_orchestration`.

use crate::addin::Addin;
use crate::generation::{OpenAttemptId, OpeningGeneration, RemovalAttemptId, RemovalEpoch};
use crate::lifecycle::{
    HostLifecycleIntent, LifecycleAccess, LifecycleCoordinator, OpenFailureDisposition,
};
use crate::module_runtime::{ModuleCleanupAuthority, ModuleClosing};
use crate::{XllError, XllResult};
use std::sync::Arc;

/// Lifecycle-only write capability issued by a [`LifecycleCoordinator`].
pub(crate) struct LifecycleControl<'coordinator, A: Addin> {
    coordinator: &'coordinator LifecycleCoordinator<A>,
}

pub(crate) type PublishGenerationFailure<A> = Box<(
    XllError,
    Option<OpeningGeneration<A>>,
    crate::module_runtime::ModuleEpochLease,
)>;

impl<'coordinator, A: Addin> LifecycleControl<'coordinator, A> {
    pub(crate) const fn new(coordinator: &'coordinator LifecycleCoordinator<A>) -> Self {
        Self { coordinator }
    }

    pub(crate) fn access(&self) -> LifecycleAccess<'_, A> {
        self.coordinator.access()
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

    pub(crate) fn claim_removal_owner(
        &self,
        control: &mut LifecycleAccess<'_, A>,
    ) -> Option<RemovalAttemptId> {
        self.coordinator.claim_removal_owner(control)
    }

    pub(crate) fn wait(&self, control: &mut LifecycleAccess<'_, A>) {
        self.coordinator.wait(control);
    }

    pub(crate) fn notify_all(&self) {
        self.coordinator.notify_all();
    }

    pub(crate) fn install_module_closing(&self, closing: ModuleClosing) {
        self.coordinator.install_module_closing(closing);
    }

    pub(crate) fn install_module_cleanup_authority(&self, authority: ModuleCleanupAuthority) {
        self.coordinator.install_module_cleanup(authority);
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
        services: Arc<crate::runtime_components::GenerationServices>,
        module_epoch: crate::module_runtime::ModuleEpochLease,
    ) -> Result<(), PublishGenerationFailure<A>> {
        self.coordinator
            .publish_opening_generation_locked(control, generation, services, module_epoch)
            .map_err(|failure| Box::new((failure.error, failure.opening, failure.module_epoch)))
    }

    pub(crate) fn finish_open_state(
        &self,
        control: &mut LifecycleAccess<'_, A>,
        generation: crate::generation::RuntimeGeneration,
    ) -> XllResult<()> {
        self.coordinator.commit_open(control, generation)
    }

    pub(crate) fn reject_open_state(&self, control: &mut LifecycleAccess<'_, A>) {
        self.coordinator.reject_open_attempt(control);
    }

    pub(crate) fn install_module_closing_locked(
        &self,
        control: &mut LifecycleAccess<'_, A>,
        closing: ModuleClosing,
    ) {
        self.coordinator
            .install_module_closing_locked(control, closing);
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

    pub(crate) fn take_module_closing_for_owner(
        &self,
        control: &mut LifecycleAccess<'_, A>,
    ) -> Option<ModuleClosing> {
        self.coordinator.take_module_closing_for_close(control)
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
