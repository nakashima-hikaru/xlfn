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
    finalization_failed: bool,
}

impl<A: crate::Addin> Drop for RemovalClaimGuard<'_, A> {
    fn drop(&mut self) {
        let lifecycle = crate::lifecycle::LifecycleControl::new(self.lifecycle);
        let mut control = lifecycle.access();
        lifecycle.release_removal_claim(&mut control, self.attempt, self.finalization_failed);
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
    pub(super) fn complete_finalization(&mut self) -> crate::XllResult<()> {
        let lifecycle = crate::lifecycle::LifecycleControl::new(self.lifecycle);
        let control = lifecycle.access();
        lifecycle.complete_boundary_finalization(&control, self.attempt)?;
        self.finalization_failed = false;
        Ok(())
    }

    pub(super) fn belongs_to(&self, lifecycle: &crate::lifecycle::LifecycleCoordinator<A>) -> bool {
        std::ptr::eq(self.lifecycle, lifecycle)
    }

    pub(crate) fn attempt(&self) -> RemovalAttemptId {
        self.attempt
    }

    pub(super) fn return_module_authority(&mut self, authority: ModuleCleanupAuthority) {
        let lifecycle = crate::lifecycle::LifecycleControl::new(self.lifecycle);
        let mut control = lifecycle.access();
        lifecycle.return_removal_authority(&mut control, self.attempt, authority);
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
                    // `closing` borrows the access obtained from these deps.
                    // The private owner bridge validates independently issued
                    // claims when it joins them to their runtime dependencies.
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

#[cfg(test)]
mod quarantine_request_tests {
    use crate::lifecycle::LifecyclePhase;
    use crate::runtime::Runtime;
    use std::sync::Arc;

    fn save_deferred_trace<A: crate::Addin>(runtime: &Runtime<A>, name: &str) {
        if let Some(directory) = std::env::var_os("XLFN_DEFERRED_TRACE_DIR") {
            std::fs::write(
                std::path::Path::new(&directory).join(name),
                runtime.composition_trace_json(),
            )
            .unwrap();
        }
    }

    #[test]
    fn quarantine_between_open_failure_and_module_abort_defers_to_the_opening_owner() {
        let runtime = Runtime::<()>::new();
        let opening = runtime.begin_open().unwrap();
        let attempt = runtime.lifecycle_control().access().open_attempt().unwrap();
        assert!(
            runtime
                .lifecycle_orchestrator()
                .mark_open_failed(attempt)
                .requires_rollback()
        );
        assert_eq!(runtime.phase(), LifecyclePhase::OpenRollbackPending);
        assert!(
            runtime
                .lifecycle_control()
                .access()
                .open_attempt()
                .is_none()
        );
        crate::runtime::recovery::quarantine_for_hazard(
            &runtime,
            crate::shutdown::UnloadHazard::AddinCleanupFailed,
        );
        assert_eq!(runtime.phase(), LifecyclePhase::Quarantined);
        assert!(matches!(
            runtime.lifecycle_control().take_module_cleanup_authority(),
            crate::module_runtime::ModuleCleanupClaim::DeferredOwner
        ));
        assert!(
            runtime
                .lifecycle_control()
                .access()
                .module_epoch_id()
                .is_none()
        );
        assert!(runtime.begin_open().is_err());
        drop(opening);
        assert_eq!(runtime.phase(), LifecyclePhase::Quarantined);
        assert!(
            runtime
                .lifecycle_control()
                .access()
                .module_epoch_id()
                .is_some()
        );
        save_deferred_trace(&runtime, "pending-open-quarantine.json");
    }

    #[test]
    fn quarantine_with_an_outstanding_removal_claim_retains_its_authority() {
        let runtime = Runtime::<PublishedOpening>::new();
        let opening = runtime.begin_open().unwrap();
        let state = Arc::new(());
        let weak = Arc::downgrade(&state);
        let mut opening = runtime.publish(opening, state, ());
        runtime.finish_open(&mut opening, Vec::new()).unwrap();
        let owner = runtime.begin_final_removal().unwrap();
        let attempt = runtime
            .lifecycle_control()
            .access()
            .removal_attempt()
            .unwrap();
        let epoch = runtime
            .lifecycle_control()
            .access()
            .module_epoch_id()
            .unwrap();
        let ingress_phase = crate::module_runtime::ingress().phase();
        crate::runtime::recovery::quarantine_for_hazard(
            &runtime,
            crate::shutdown::UnloadHazard::AddinCleanupFailed,
        );
        assert_eq!(runtime.phase(), LifecyclePhase::Quarantined);
        assert_eq!(
            runtime.lifecycle_control().access().removal_attempt(),
            Some(attempt)
        );
        assert_eq!(
            runtime.lifecycle_control().access().module_epoch_id(),
            Some(epoch)
        );
        assert_eq!(crate::module_runtime::ingress().phase(), ingress_phase);
        assert!(weak.upgrade().is_some());
        assert!(matches!(
            runtime.lifecycle_control().take_module_cleanup_authority(),
            crate::module_runtime::ModuleCleanupClaim::DeferredOwner
        ));
        drop(owner);
        assert!(
            runtime
                .lifecycle_control()
                .access()
                .removal_attempt()
                .is_none()
        );
        assert_eq!(
            runtime.lifecycle_control().access().module_epoch_id(),
            Some(epoch)
        );
        assert!(weak.upgrade().is_some());
        save_deferred_trace(&runtime, "outstanding-owner-quarantine.json");
    }

    fn assert_quarantine_trace<A: crate::Addin>(
        runtime: &Runtime<A>,
        attempt: crate::generation::OpenAttemptId,
    ) {
        let composition = runtime.composition_trace_json();
        let document: serde_json::Value = serde_json::from_str(&composition).unwrap();
        assert_eq!(document["outcome"], "quarantined");
        let events = document["events"].as_array().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.get("quarantineOpen").is_some())
                .count(),
            1
        );
        assert_eq!(
            events.last().unwrap()["quarantineOpen"]["attempt"],
            attempt.get()
        );
        let shutdown: serde_json::Value =
            serde_json::from_str(&runtime.shutdown_trace_json()).unwrap();
        assert_eq!(shutdown["outcome"], "quarantined");
        assert_eq!(shutdown["attempt"], attempt.get());
        #[cfg(not(miri))]
        for (variable, trace) in [
            ("XLFN_COMPOSITION_CHECKER", composition),
            ("XLFN_SHUTDOWN_CHECKER", runtime.shutdown_trace_json()),
        ] {
            if let Some(checker) = std::env::var_os(variable) {
                use std::io::Write;
                use std::process::{Command, Stdio};
                let mut child = Command::new(checker)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap();
                child
                    .stdin
                    .take()
                    .unwrap()
                    .write_all(trace.as_bytes())
                    .unwrap();
                let output = child.wait_with_output().unwrap();
                assert!(
                    output.status.success(),
                    "{variable} rejected deferred quarantine: {} {}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
    }

    #[test]
    fn miri_quarantine_request_waits_for_the_unpublished_opening_owner() {
        let runtime = Runtime::<()>::new();
        let opening = runtime.begin_open().unwrap();
        let attempt = runtime.lifecycle_control().access().open_attempt().unwrap();
        assert!(matches!(
            runtime.lifecycle_control().take_module_cleanup_authority(),
            crate::module_runtime::ModuleCleanupClaim::Unavailable,
        ));
        crate::runtime::recovery::quarantine_for_hazard(
            &runtime,
            crate::shutdown::UnloadHazard::AddinCleanupFailed,
        );
        assert_eq!(runtime.phase(), LifecyclePhase::Closing);
        assert_eq!(
            runtime.lifecycle_control().access().open_attempt(),
            Some(attempt)
        );
        assert!(runtime.begin_open().is_err());
        assert_quarantine_trace(&runtime, attempt);
        let trace = runtime.composition_trace_json();
        crate::runtime::recovery::quarantine_for_hazard(
            &runtime,
            crate::shutdown::UnloadHazard::AddinCleanupFailed,
        );
        assert_eq!(runtime.composition_trace_json(), trace);
        drop(opening);
        assert_eq!(runtime.phase(), LifecyclePhase::Quarantined);
        assert!(
            runtime
                .lifecycle_control()
                .access()
                .open_attempt()
                .is_none()
        );
        assert!(runtime.begin_open().is_err());
        assert_quarantine_trace(&runtime, attempt);
    }

    struct PublishedOpening;
    impl crate::Addin for PublishedOpening {
        #[cfg(feature = "rtd")]
        type RtdSubscription = ();
        #[cfg(feature = "rtd")]
        type RtdSources = crate::DynamicRtdSources<()>;
        type SharedState = Arc<()>;
        type LifecycleState = ();
        type Error = crate::XllError;
        type Layers = ();
        #[cfg(feature = "async")]
        type AsyncExecutor = <() as crate::Addin>::AsyncExecutor;
        fn open(_: &crate::OpenContext<Self>) -> crate::OpenResult<Self> {
            Ok(crate::Opened::new(Arc::new(())))
        }
    }

    #[test]
    fn miri_quarantine_request_retains_published_opening_until_owner_resolution() {
        let runtime = Runtime::<PublishedOpening>::new();
        let opening = runtime.begin_open().unwrap();
        let state = Arc::new(());
        let weak = Arc::downgrade(&state);
        let opening = runtime.publish(opening, state, ());
        let attempt = runtime.lifecycle_control().access().open_attempt().unwrap();
        crate::runtime::recovery::quarantine_for_hazard(
            &runtime,
            crate::shutdown::UnloadHazard::AddinCleanupFailed,
        );
        assert_eq!(runtime.phase(), LifecyclePhase::Closing);
        assert_eq!(
            runtime.lifecycle_control().access().open_attempt(),
            Some(attempt)
        );
        assert!(weak.upgrade().is_some());
        let lifecycle = runtime.lifecycle_control();
        let mut access = lifecycle.access();
        let crate::lifecycle::FinalCloseDecision::Closing(closing) = access.final_close_decision()
        else {
            panic!("quarantine request retains the canonical closing state");
        };
        assert!(closing.try_claim().is_none());
        drop(access);
        drop(opening);
        assert_eq!(runtime.phase(), LifecyclePhase::Quarantined);
        assert!(
            runtime
                .lifecycle_control()
                .access()
                .open_attempt()
                .is_none()
        );
        assert!(weak.upgrade().is_some());
        assert_quarantine_trace(&runtime, attempt);
    }
}
