//! The narrow write capability for canonical lifecycle state.
//!
//! `LifecycleControl` deliberately owns no `Runtime` reference. It can
//! mutate only the lifecycle coordinator; runtime-wide orchestration belongs
//! to `crate::runtime::orchestration`.

use crate::addin_impl::Addin;
use crate::generation::{OpenAttemptId, OpeningGeneration, RemovalAttemptId, RemovalEpoch};
use crate::lifecycle::{
    CanonicalGenerationPublished, FinalRemovalReady, HostLifecycleIntent, LifecycleAccess,
    LifecycleCoordinator, OpenFailureDisposition, OpenRollbackReady, UnpublishedGeneration,
};
use crate::module_runtime::{ModuleCleanupAuthority, ModuleCleanupClaim, ModuleClosing};
use crate::{XllError, XllResult};

/// Lifecycle-only write capability issued by a [`LifecycleCoordinator`].
pub(crate) struct LifecycleControl<'coordinator, A: Addin> {
    coordinator: &'coordinator LifecycleCoordinator<A>,
}

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

    pub(crate) fn clear_certified_retirement(
        &self,
        control: LifecycleAccess<'_, A>,
        authorization: crate::runtime::CertifiedRetirement<'_, A>,
    ) -> bool {
        self.coordinator
            .clear_certified_retirement(control, authorization)
    }

    pub(crate) fn begin_open(
        &self,
        expected_removal_epoch: RemovalEpoch,
    ) -> XllResult<super::OpenStart<'coordinator, A>> {
        super::OpenStart::acquire(self.coordinator, expected_removal_epoch)
    }

    pub(crate) fn record_open_failure(
        &self,
        control: &mut LifecycleAccess<'_, A>,
    ) -> OpenFailureDisposition {
        self.coordinator.record_open_failure(control)
    }

    pub(crate) fn notify_all(&self) {
        self.coordinator.notify_all();
    }

    pub(crate) fn release_removal_claim(
        &self,
        control: &mut LifecycleAccess<'_, A>,
        attempt: RemovalAttemptId,
        finalization_failed: bool,
    ) {
        self.coordinator
            .release_removal_claim(control, attempt, finalization_failed);
    }

    pub(crate) fn return_removal_authority(
        &self,
        control: &mut LifecycleAccess<'_, A>,
        attempt: RemovalAttemptId,
        authority: ModuleCleanupAuthority,
    ) {
        self.coordinator
            .return_removal_authority(control, attempt, authority.into_authority());
    }

    pub(crate) fn finish_final_removal(
        &self,
        control: &mut LifecycleAccess<'_, A>,
        authorization: crate::runtime::CanonicalCompletion<'_, A, crate::runtime::FinalRemoval>,
    ) -> XllResult<()> {
        self.coordinator
            .finish_final_removal(control, authorization)
    }

    pub(crate) fn finish_open_rollback(
        &self,
        control: &mut LifecycleAccess<'_, A>,
        authorization: crate::runtime::CanonicalCompletion<'_, A, crate::runtime::OpenRollback>,
    ) -> XllResult<()> {
        self.coordinator
            .finish_open_rollback(control, authorization)
    }

    pub(crate) fn complete_boundary_finalization(
        &self,
        control: &LifecycleAccess<'_, A>,
        attempt: RemovalAttemptId,
    ) -> XllResult<()> {
        self.coordinator
            .complete_boundary_finalization(control, attempt)
    }

    #[allow(
        clippy::result_large_err,
        reason = "Rollback returns the complete affine generation, including RTD limits, without a new allocation"
    )]
    pub(crate) fn stage_opening_generation(
        &self,
        attempt_id: OpenAttemptId,
        opening: OpeningGeneration<A>,
    ) -> Result<(), (XllError, OpeningGeneration<A>)> {
        let mut control = self.coordinator.access();
        if control.open_attempt() != Some(attempt_id) {
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
        attempt: OpenAttemptId,
        unpublished: UnpublishedGeneration<A>,
    ) -> Result<CanonicalGenerationPublished<'coordinator, A>, Box<UnpublishedGeneration<A>>> {
        self.coordinator
            .publish_opening_generation_locked(control, attempt, unpublished)
    }

    pub(crate) fn finish_open_state<'access, 'issuer>(
        &self,
        control: &'access mut LifecycleAccess<'issuer, A>,
        published: CanonicalGenerationPublished<'_, A>,
    ) -> XllResult<super::CommittedOpen<'access, 'issuer, A>> {
        self.coordinator.commit_open(control, published)
    }

    pub(crate) fn complete_open_abort_locked(
        &self,
        control: &mut LifecycleAccess<'_, A>,
        closing: ModuleClosing,
    ) -> Result<(), crate::module_runtime::ModuleAuthority> {
        self.coordinator
            .complete_open_abort_locked(control, closing)
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

    pub(crate) fn take_module_cleanup_authority(&self) -> ModuleCleanupClaim {
        self.coordinator.take_module_cleanup_for_quarantine()
    }

    pub(crate) fn retain_failed_module_cleanup(
        &self,
        authority: ModuleCleanupAuthority,
    ) -> Result<(), crate::module_runtime::ModuleAuthority> {
        self.coordinator.retain_failed_module_cleanup(authority)
    }

    pub(crate) fn take_opening_for_rollback(&self) -> Option<OpeningGeneration<A>> {
        self.coordinator.take_opening_for_rollback()
    }
}
