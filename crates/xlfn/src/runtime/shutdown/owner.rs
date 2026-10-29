use super::{FinalRemoval, OpenRollback};
use crate::XllError;
use crate::generation::RemovalAttemptId;
use crate::lifecycle::{LifecycleControl, LifecyclePhase};
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
}

impl<A: crate::Addin> Drop for RemovalClaimGuard<'_, A> {
    fn drop(&mut self) {
        let lifecycle = crate::lifecycle::LifecycleControl::new(self.lifecycle);
        let mut control = lifecycle.access();
        let returned = self
            .returned
            .take()
            .map(|authority| authority.into_authority());
        lifecycle.release_removal_claim(&mut control, self.attempt, returned);
        self.observer.release_cleanup_owner();
        lifecycle.notify_all();
    }
}

impl<'runtime, A: crate::Addin, K> RemovalOwner<'runtime, A, K> {
    fn new(deps: ShutdownDeps<'runtime, A>, claim: crate::lifecycle::RemovalClaim) -> Self {
        let guard = RemovalClaimGuard {
            lifecycle: deps.lifecycle(),
            observer: deps.observer(),
            attempt: claim.attempt(),
            returned: None,
        };
        Self {
            deps,
            state: Some((guard, claim.into_module_closing())),
            _kind: std::marker::PhantomData,
        }
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
        let lifecycle = LifecycleControl::new(deps.lifecycle());
        let mut wait_guard = lifecycle.access();
        lifecycle.begin_removal_request(&mut wait_guard);
        deps.returns().close_admission();
        let mut request_recorded = false;

        'retry: loop {
            let decision = 'decision: {
                match wait_guard.phase() {
                    LifecyclePhase::Closed => {
                        if wait_guard.removal_attempt().is_none()
                            && deps.returns().returns_are_quiescent()
                        {
                            deps.observer().request_final_close(&mut request_recorded);
                            break 'decision Some(());
                        }
                        if wait_guard.removal_attempt().is_none() {
                            drop(wait_guard);
                            deps.returns().wait_for_returns();
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
                    LifecyclePhase::Quarantined => break 'decision Some(()),
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
                    deps.observer().request_final_close(&mut request_recorded);
                }

                if wait_guard.phase() != LifecyclePhase::Closed
                    && wait_guard.open_attempt().is_none()
                    && let Some(claim) = lifecycle.claim_removal(&mut wait_guard)
                {
                    let owner = Self::new(deps, claim);
                    drop(wait_guard);
                    deps.observer().acquire_final_close_owner();
                    return Some(owner);
                } else {
                    None
                }
            };

            match decision {
                Some(()) => return None,
                None => lifecycle.wait(&mut wait_guard),
            }
        }
    }
}

impl<'runtime, A: crate::Addin> RemovalOwner<'runtime, A, OpenRollback> {
    pub(in crate::runtime) fn acquire_rollback(
        deps: ShutdownDeps<'runtime, A>,
    ) -> OpenRollbackAcquisition<Self> {
        let lifecycle = LifecycleControl::new(deps.lifecycle());
        let mut wait_guard = lifecycle.access();
        loop {
            match wait_guard.phase() {
                LifecyclePhase::Closed | LifecyclePhase::Closing => {
                    return OpenRollbackAcquisition::ClosingOwnsCleanup;
                }
                LifecyclePhase::OpenRollbackPending => {}
                LifecyclePhase::Opening | LifecyclePhase::Open | LifecyclePhase::Quarantined => {
                    return OpenRollbackAcquisition::Unavailable;
                }
            }
            if let Some(claim) = lifecycle.claim_removal(&mut wait_guard) {
                let owner = Self::new(deps, claim);
                drop(wait_guard);
                deps.observer().acquire_open_rollback_owner();
                return OpenRollbackAcquisition::Acquired(owner);
            }
            lifecycle.wait(&mut wait_guard);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
