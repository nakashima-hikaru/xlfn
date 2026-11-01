use super::{FinalRemoval, OpenRollback};
use crate::generation::RemovalAttemptId;
use crate::lifecycle::{FinalCloseDecision, IssuedRemovalClaim, RollbackDecision};
use crate::module_runtime::{ModuleCleanupAuthority, ModuleClosing};
use crate::runtime::capabilities::ShutdownDeps;
use crate::runtime::orchestration::OpenRollbackAcquisition;

/// The initial removal claim and its unique, undrained module capability.
/// Draining consumes this value, so a caller cannot drain the same claim twice.
pub(crate) struct RemovalOwner<'runtime, A: crate::Addin, K> {
    deps: ShutdownDeps<'runtime, A>,
    state: Option<(RemovalClaimGuard<'runtime, A>, ModuleClosing)>,
    _kind: std::marker::PhantomData<K>,
}

/// Keeps cleanup ownership until the terminal callback has returned. The
/// pipeline owns module authority separately and returns it here on failure.
pub(crate) struct RemovalClaimGuard<'runtime, A: crate::Addin> {
    lifecycle: &'runtime crate::lifecycle::LifecycleCoordinator<A>,
    observer: &'runtime crate::runtime::observer::RuntimeObserver,
    attempt: RemovalAttemptId,
    returned: Option<Box<ModuleCleanupAuthority>>,
    finalization_failed: bool,
}

impl<A: crate::Addin> Drop for RemovalClaimGuard<'_, A> {
    fn drop(&mut self) {
        let lifecycle = crate::lifecycle::LifecycleControl::new(self.lifecycle);
        let mut control = lifecycle.access();
        let returned = self
            .returned
            .take()
            .map(|authority| authority.into_authority());
        lifecycle.release_removal_claim(
            &mut control,
            self.attempt,
            returned,
            self.finalization_failed,
        );
        self.observer.release_cleanup_owner();
        lifecycle.notify_all();
    }
}

impl<'runtime, A: crate::Addin, K> RemovalOwner<'runtime, A, K> {
    fn new(
        deps: ShutdownDeps<'runtime, A>,
        claim: IssuedRemovalClaim<'runtime, A, K>,
    ) -> Result<Self, IssuedRemovalClaim<'runtime, A, K>> {
        let (attempt, closing) = claim.into_parts(deps.lifecycle())?;
        let guard = RemovalClaimGuard {
            lifecycle: deps.lifecycle(),
            observer: deps.observer(),
            attempt,
            returned: None,
            finalization_failed: false,
        };
        Ok(Self {
            deps,
            state: Some((guard, closing)),
            _kind: std::marker::PhantomData,
        })
    }

    pub(super) fn into_parts(
        mut self,
    ) -> (
        ShutdownDeps<'runtime, A>,
        RemovalClaimGuard<'runtime, A>,
        ModuleClosing,
    ) {
        let (guard, closing) = self.state.take().expect("removal owner consumed once");
        (self.deps, guard, closing)
    }

    #[cfg(test)]
    pub(crate) fn into_test_parts(self) -> (RemovalClaimGuard<'runtime, A>, ModuleClosing) {
        let (_, guard, closing) = self.into_parts();
        (guard, closing)
    }
}

impl<A: crate::Addin, K> Drop for RemovalOwner<'_, A, K> {
    fn drop(&mut self) {
        if let Some((mut guard, closing)) = self.state.take() {
            guard.return_module_authority(ModuleCleanupAuthority::Closing(closing));
        }
    }
}

impl<A: crate::Addin> RemovalClaimGuard<'_, A> {
    pub(super) fn mark_finalization_failed(&mut self) {
        self.finalization_failed = true;
    }
    pub(super) fn complete_finalization(&mut self) {
        self.finalization_failed = false;
    }

    pub(super) fn belongs_to(&self, lifecycle: &crate::lifecycle::LifecycleCoordinator<A>) -> bool {
        std::ptr::eq(self.lifecycle, lifecycle)
    }

    pub(crate) fn attempt(&self) -> RemovalAttemptId {
        self.attempt
    }

    pub(super) fn return_module_authority(&mut self, authority: ModuleCleanupAuthority) {
        if self.returned.replace(Box::new(authority)).is_some() {
            xlfn_kernel::invariant::fail_stop();
        }
    }
}

impl<'runtime, A: crate::Addin> RemovalOwner<'runtime, A, FinalRemoval> {
    pub(in crate::runtime) fn acquire_final(deps: ShutdownDeps<'runtime, A>) -> Option<Self> {
        let mut wait_guard = deps.lifecycle().access();
        wait_guard.begin_final_removal_request();
        deps.returns().close_admission();
        let mut request_recorded = false;

        loop {
            match wait_guard.final_close_decision() {
                FinalCloseDecision::ClosedIdle => {
                    if deps.returns().returns_are_quiescent() {
                        deps.observer().request_final_close(&mut request_recorded);
                        return None;
                    }
                    drop(wait_guard);
                    deps.returns().wait_for_returns();
                    wait_guard = deps.lifecycle().access();
                    continue;
                }
                FinalCloseDecision::Quarantined => return None,
                FinalCloseDecision::Wait => {
                    deps.observer().request_final_close(&mut request_recorded);
                }
                FinalCloseDecision::Closing(closing) => {
                    // Identity is checked before the observer records anything.
                    if !closing.belongs_to(deps.lifecycle()) {
                        xlfn_kernel::invariant::fail_stop();
                    }
                    deps.observer().request_final_close(&mut request_recorded);
                    if let Some(claim) = closing.try_claim() {
                        let owner = Self::new(deps, claim)
                            .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop());
                        drop(wait_guard);
                        deps.observer().acquire_final_close_owner();
                        return Some(owner);
                    }
                }
            }
            wait_guard.wait_for_change();
        }
    }
}

impl<'runtime, A: crate::Addin> RemovalOwner<'runtime, A, OpenRollback> {
    pub(in crate::runtime) fn acquire_rollback(
        deps: ShutdownDeps<'runtime, A>,
    ) -> OpenRollbackAcquisition<Self> {
        let mut wait_guard = deps.lifecycle().access();
        loop {
            match wait_guard.rollback_decision() {
                RollbackDecision::ClosedIdle => {
                    // These are read-side checks, not callbacks. Keep the
                    // canonical lock until this one result is classified.
                    let finalized = deps.host().generation_callbacks_detached()
                        && !deps.host().registration_state_unknown()
                        && deps.residency().callbacks_retained_safely()
                        && crate::excel_rtd_protocol::logical_quiescence_certified();
                    return if finalized {
                        OpenRollbackAcquisition::AlreadyFinalized
                    } else {
                        OpenRollbackAcquisition::ClosingOwnsCleanup
                    };
                }
                RollbackDecision::ClosingOwnsCleanup => {
                    return OpenRollbackAcquisition::ClosingOwnsCleanup;
                }
                RollbackDecision::Unavailable => {
                    return OpenRollbackAcquisition::Unavailable;
                }
                RollbackDecision::Wait => {}
                RollbackDecision::Claim(claim) => {
                    let owner = Self::new(deps, claim)
                        .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop());
                    drop(wait_guard);
                    deps.observer().acquire_open_rollback_owner();
                    return OpenRollbackAcquisition::Acquired(owner);
                }
            }
            wait_guard.wait_for_change();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::LifecyclePhase;
    use crate::runtime::Runtime;

    fn assert_drop_returns_to_issuer<K>(
        runtime: &Runtime<()>,
        other: &Runtime<()>,
        owner: RemovalOwner<'_, (), K>,
    ) -> RemovalAttemptId {
        let (guard, closing) = owner.state.as_ref().unwrap();
        assert!(guard.belongs_to(&runtime.lifecycle));
        assert!(!guard.belongs_to(&other.lifecycle));
        assert!(std::ptr::eq(
            guard.observer,
            runtime.shutdown_deps().observer()
        ));
        assert!(!std::ptr::eq(
            guard.observer,
            other.shutdown_deps().observer()
        ));
        let attempt = guard.attempt();
        let epoch = closing.id();
        assert_eq!(runtime.lifecycle.access().removal_attempt(), Some(attempt));
        assert!(other.lifecycle.access().removal_attempt().is_none());
        let own_trace = runtime.composition_trace_json();
        let released = own_trace.matches("releaseCleanupOwner").count();
        assert_eq!(
            own_trace.matches("acquireFinalCloseOwner").count()
                + own_trace.matches("acquireOpenRollbackOwner").count(),
            released + 1,
        );
        let other_trace = other.composition_trace_json();
        drop(owner);
        assert_eq!(
            runtime
                .composition_trace_json()
                .matches("releaseCleanupOwner")
                .count(),
            released + 1,
        );
        assert_eq!(other.composition_trace_json(), other_trace);
        assert!(runtime.lifecycle.access().removal_attempt().is_none());
        assert_eq!(runtime.lifecycle.access().module_epoch_id(), Some(epoch));
        assert_eq!(other.phase(), LifecyclePhase::Closed);
        assert!(other.lifecycle.access().module_epoch_id().is_none());
        attempt
    }

    #[test]
    fn abandoned_acquisition_returns_authority_to_its_original_issuer() {
        for rollback in [false, true] {
            let runtime = Runtime::<()>::new();
            let other = Runtime::<()>::new();
            let opening = runtime.begin_open().unwrap();
            if rollback {
                assert!(opening.fail_for_test().requires_rollback());
                let OpenRollbackAcquisition::Acquired(owner) =
                    RemovalOwner::acquire_rollback(runtime.shutdown_deps())
                else {
                    panic!("rollback acquisition unavailable");
                };
                let first = assert_drop_returns_to_issuer(&runtime, &other, owner);
                let OpenRollbackAcquisition::Acquired(owner) =
                    RemovalOwner::acquire_rollback(runtime.shutdown_deps())
                else {
                    panic!("abandoned rollback did not return authority");
                };
                assert_ne!(
                    first,
                    assert_drop_returns_to_issuer(&runtime, &other, owner)
                );
            } else {
                let mut opening = runtime.publish(opening, (), ());
                runtime.finish_open(&mut opening, Vec::new()).unwrap();
                let owner = RemovalOwner::acquire_final(runtime.shutdown_deps()).unwrap();
                let first = assert_drop_returns_to_issuer(&runtime, &other, owner);
                let owner = RemovalOwner::acquire_final(runtime.shutdown_deps()).unwrap();
                assert_ne!(
                    first,
                    assert_drop_returns_to_issuer(&runtime, &other, owner)
                );
            }
        }
    }

    fn reject_foreign_issuer<'a, K>(
        runtime: &'a Runtime<()>,
        other: &'a Runtime<()>,
        claim: IssuedRemovalClaim<'a, (), K>,
    ) -> IssuedRemovalClaim<'a, (), K> {
        let own_trace = runtime.composition_trace_json();
        let other_trace = other.composition_trace_json();
        let own_attempt = runtime.lifecycle.access().removal_attempt();
        let Err(claim) = RemovalOwner::<(), K>::new(other.shutdown_deps(), claim) else {
            panic!("a foreign issuer must not acquire this claim");
        };
        assert_eq!(runtime.composition_trace_json(), own_trace);
        assert_eq!(other.composition_trace_json(), other_trace);
        assert_eq!(runtime.lifecycle.access().removal_attempt(), own_attempt);
        assert!(other.lifecycle.access().removal_attempt().is_none());
        claim
    }

    #[test]
    fn foreign_issuer_rejection_retains_each_purpose_for_its_original_owner() {
        let runtime = Runtime::<()>::new();
        let other = Runtime::<()>::new();
        let opening = runtime.begin_open().unwrap();
        let mut opening = runtime.publish(opening, (), ());
        runtime.finish_open(&mut opening, Vec::new()).unwrap();
        let mut access = runtime.lifecycle.access();
        access.begin_final_removal_request();
        runtime.shutdown_deps().returns().close_admission();
        let FinalCloseDecision::Closing(closing) = access.final_close_decision() else {
            panic!("opened runtime must enter canonical closing");
        };
        assert!(closing.belongs_to(&runtime.lifecycle));
        assert!(!closing.belongs_to(&other.lifecycle));
        runtime.observer().request_final_close(&mut false);
        let claim = closing.try_claim().unwrap();
        drop(access);
        let claim = reject_foreign_issuer(&runtime, &other, claim);
        let owner = RemovalOwner::new(runtime.shutdown_deps(), claim)
            .unwrap_or_else(|_| panic!("original issuer rejected its own final claim"));
        runtime.observer().acquire_final_close_owner();
        assert_drop_returns_to_issuer(&runtime, &other, owner);
        drop(opening);
        drop(runtime);

        let rollback_runtime = Runtime::<()>::new();
        let opening = rollback_runtime.begin_open().unwrap();
        assert!(opening.fail_for_test().requires_rollback());
        let mut access = rollback_runtime.lifecycle.access();
        let RollbackDecision::Claim(claim) = access.rollback_decision() else {
            panic!("failed opening must issue a rollback claim");
        };
        drop(access);
        let claim = reject_foreign_issuer(&rollback_runtime, &other, claim);
        let owner = RemovalOwner::new(rollback_runtime.shutdown_deps(), claim)
            .unwrap_or_else(|_| panic!("original issuer rejected its own rollback claim"));
        rollback_runtime.observer().acquire_open_rollback_owner();
        assert_drop_returns_to_issuer(&rollback_runtime, &other, owner);
    }
}
