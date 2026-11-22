//! Boundary completion owns both logical completion and the exact binding.
//! Dropping/forgetting intermediate work never issues a finalized result.
use super::certificate::{FinalRemoval, OpenRollback, TerminalCertificateKind};
use crate::runtime::capabilities::{EmptyLifecycle, LifecycleFailure, ShutdownDeps};
use crate::{Addin, XllError};
use xlfn_kernel::rollback::{Rollback, RollbackGuard};

#[cfg(test)]
std::thread_local! {
    static PANIC_DURING_ARM: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static QUARANTINE_AFTER_BINDING_RELEASE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(super) fn check_arm_fault() {
    assert!(
        !PANIC_DURING_ARM.with(|fault| fault.replace(false)),
        "injected panic while arming finalization"
    );
}

#[must_use = "finish logical closure and release the retained binding"]
pub(crate) struct CompletionPendingBinding<'runtime, A: Addin, K: TerminalCertificateKind> {
    certified: RollbackGuard<CertifiedBinding<'runtime, A, K>>,
}

struct CertifiedBinding<'runtime, A: Addin, K: TerminalCertificateKind> {
    deps: ShutdownDeps<'runtime, A>,
    certificate: K::Certificate<'runtime, A>,
    empty: EmptyLifecycle<'runtime, A>,
}

impl<A: Addin, K: TerminalCertificateKind> Rollback for CertifiedBinding<'_, A, K> {
    fn rollback(self) {
        // Publish quarantine while the exact certificate still retains the claim.
        self.deps.quarantine_state();
    }
}

/// The certificate's removal claim is armed for quarantine before this issues.
struct ArmedFinalization<'runtime, A: Addin, K: TerminalCertificateKind>(
    CertifiedBinding<'runtime, A, K>,
);

struct LogicalCompleted<'runtime, A: Addin, K: TerminalCertificateKind> {
    deps: ShutdownDeps<'runtime, A>,
    completed: K::Completed<'runtime, A>,
    empty: EmptyLifecycle<'runtime, A>,
}

struct BindingReleased<'runtime, A: Addin, K: TerminalCertificateKind> {
    deps: ShutdownDeps<'runtime, A>,
    completed: K::Completed<'runtime, A>,
}

#[allow(
    dead_code,
    reason = "failure variants retain their distinct affine recoveries"
)]
enum FailedFinalization<'runtime, A: Addin, K: TerminalCertificateKind> {
    Armed(ArmedFinalization<'runtime, A, K>),
    Release {
        completed: K::Completed<'runtime, A>,
        binding: LifecycleFailure<'runtime, A>,
    },
    Boundary(BindingReleased<'runtime, A, K>),
}

type FinalizationResult<'runtime, A, K, T> = Result<T, Box<FinalizationFailure<'runtime, A, K>>>;

#[must_use = "the failure retains the progressed completion and binding responsibilities"]
pub(crate) struct FinalizationFailure<'runtime, A: Addin, K: TerminalCertificateKind> {
    error: XllError,
    recovery: FailedFinalization<'runtime, A, K>,
}
impl<A: Addin, K: TerminalCertificateKind> FinalizationFailure<'_, A, K> {
    pub(crate) fn quarantine(self) -> XllError {
        let Self { error, recovery } = self;
        drop(recovery);
        error
    }
}

/// This proof exists only after logical commit AND successful binding release.
pub(crate) struct Finalized<'runtime, A: Addin, K: TerminalCertificateKind> {
    deps: ShutdownDeps<'runtime, A>,
    completed: K::Completed<'runtime, A>,
}
pub(crate) type FinalizedRemoval<'runtime, A> = Finalized<'runtime, A, FinalRemoval>;

impl<'runtime, A: Addin, K: TerminalCertificateKind> CompletionPendingBinding<'runtime, A, K> {
    pub(super) fn new(
        deps: ShutdownDeps<'runtime, A>,
        certificate: K::Certificate<'runtime, A>,
        empty: EmptyLifecycle<'runtime, A>,
    ) -> Self {
        Self {
            certified: RollbackGuard::new(CertifiedBinding {
                deps,
                certificate,
                empty,
            }),
        }
    }

    pub(crate) fn finish(self) -> FinalizationResult<'runtime, A, K, Finalized<'runtime, A, K>> {
        self.arm_finalization()
            .finish_logical()?
            .release_binding()?
            .finalize()
    }

    fn arm_finalization(mut self) -> ArmedFinalization<'runtime, A, K> {
        K::arm_finalization(&mut self.certified.get_mut().certificate);
        ArmedFinalization(self.certified.complete())
    }
}

impl<'runtime, A: Addin, K: TerminalCertificateKind> ArmedFinalization<'runtime, A, K> {
    fn finish_logical(
        self,
    ) -> FinalizationResult<'runtime, A, K, LogicalCompleted<'runtime, A, K>> {
        let CertifiedBinding {
            deps,
            certificate,
            empty,
        } = self.0;
        match K::finish(certificate) {
            Ok(completed) => Ok(LogicalCompleted {
                deps,
                completed,
                empty,
            }),
            Err((error, certificate)) => Err(Box::new(FinalizationFailure {
                error,
                recovery: FailedFinalization::Armed(Self(CertifiedBinding {
                    deps,
                    certificate: *certificate,
                    empty,
                })),
            })),
        }
    }
}

impl<'runtime, A: Addin, K: TerminalCertificateKind> LogicalCompleted<'runtime, A, K> {
    fn release_binding(
        self,
    ) -> FinalizationResult<'runtime, A, K, BindingReleased<'runtime, A, K>> {
        let Self {
            deps,
            completed,
            empty,
        } = self;
        match empty.release() {
            Ok(()) => Ok(BindingReleased { deps, completed }),
            Err(binding) => Err(Box::new(FinalizationFailure {
                error: binding.error().clone(),
                recovery: FailedFinalization::Release { completed, binding },
            })),
        }
    }
}

impl<'runtime, A: Addin, K: TerminalCertificateKind> BindingReleased<'runtime, A, K> {
    fn finalize(mut self) -> FinalizationResult<'runtime, A, K, Finalized<'runtime, A, K>> {
        if let Err(error) = K::record_binding_release(&self.completed) {
            return Err(Box::new(FinalizationFailure {
                error,
                recovery: FailedFinalization::Boundary(self),
            }));
        }
        #[cfg(test)]
        if QUARANTINE_AFTER_BINDING_RELEASE.with(|request| request.replace(false)) {
            self.deps.quarantine_state();
        }
        if let Err(error) = K::complete_finalization(&mut self.completed) {
            return Err(Box::new(FinalizationFailure {
                error,
                recovery: FailedFinalization::Boundary(self),
            }));
        }
        Ok(Finalized {
            deps: self.deps,
            completed: self.completed,
        })
    }
}
impl<'runtime, A: Addin> Finalized<'runtime, A, FinalRemoval> {
    pub(crate) fn record_return(self) {
        let (witness, _claim) = self.completed;
        self.deps.observer().mark_returned_success();
        if witness.is_committed() {
            self.deps.observer().retire_committed_shutdown();
        }
        self.deps.observer().mark_return_pending();
    }
}
// Open rollback needs no success payload outside the owning completion value.
impl<A: Addin> std::fmt::Debug for Finalized<'_, A, OpenRollback> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FinalizedRollback")
    }
}

#[cfg(test)]
mod tests {
    use super::super::pipeline::{AddinQuiesced, TeardownTxn};
    use super::*;
    use crate::lifecycle::LifecyclePhase;
    use crate::runtime::{AddinLifecycleAccess, Runtime};

    fn pending(runtime: &Runtime<()>) -> CompletionPendingBinding<'_, (), FinalRemoval> {
        let opening = runtime.begin_open().unwrap();
        let opening = runtime.publish(opening, (), ());
        runtime.finish_open(opening, Vec::new()).unwrap();
        let lifecycle = runtime.bind_addin_lifecycle().unwrap();
        let teardown = runtime
            .begin_final_removal()
            .unwrap()
            .drain()
            .unwrap_or_else(|_| panic!("drain"))
            .stop_producers(|_| {})
            .unwrap_or_else(|_| panic!("stop"))
            .close_module_callbacks()
            .detach_host()
            .unwrap_or_else(|_| panic!("detach"))
            .quiesce_addin(runtime, &lifecycle)
            .unwrap_or_else(|_| panic!("quiesce"));
        certify_quiesced(teardown, &lifecycle)
    }

    fn pending_rollback(runtime: &Runtime<()>) -> CompletionPendingBinding<'_, (), OpenRollback> {
        let opening = runtime.begin_open().unwrap();
        let generation = opening.attempt_id().into_runtime_generation();
        let opening = runtime.publish(opening, (), ());
        assert!(opening.fail_for_test().requires_rollback());
        let lifecycle = runtime.bind_addin_lifecycle().unwrap();
        let crate::runtime::orchestration::OpenRollbackAcquisition::Acquired(owner) =
            runtime.acquire_open_rollback()
        else {
            panic!("rollback cleanup owner unavailable");
        };
        let teardown = owner
            .drain()
            .unwrap_or_else(|_| panic!("drain"))
            .stop_producers(|_| {})
            .unwrap_or_else(|_| panic!("stop"))
            .close_module_callbacks()
            .detach_host()
            .unwrap_or_else(|_| panic!("detach"))
            .quiesce_addin(runtime, &lifecycle, Some(generation))
            .unwrap_or_else(|_| panic!("quiesce"));
        certify_quiesced(teardown, &lifecycle)
    }

    fn certify_quiesced<'runtime, K: TerminalCertificateKind>(
        teardown: TeardownTxn<'runtime, (), K, AddinQuiesced<'runtime, ()>>,
        lifecycle: &AddinLifecycleAccess<'_, ()>,
    ) -> CompletionPendingBinding<'runtime, (), K> {
        teardown
            .seal_services()
            .unwrap_or_else(|_| panic!("seal"))
            .cleanup_addin(lifecycle, &mut crate::shutdown::CloseReport::default())
            .unwrap_or_else(|_| panic!("cleanup"))
            .finish_services()
            .unwrap_or_else(|_| panic!("services"))
            .reclaim(|_| {})
            .unwrap_or_else(|_| panic!("reclaim"))
            .certify()
            .unwrap_or_else(|_| panic!("certify"))
    }

    #[test]
    fn completion_keeps_binding_and_claim_until_ordered_finalization() {
        let runtime = Runtime::new();
        let pending = pending(&runtime);
        let access = runtime.bind_addin_lifecycle().unwrap();
        assert_eq!(runtime.phase(), LifecyclePhase::Closing);
        assert!(
            runtime
                .shutdown_deps()
                .empty_addin_lifecycle(&access)
                .is_err()
        );
        let finalized = pending.finish().unwrap_or_else(|_| panic!("finish"));
        assert_eq!(runtime.phase(), LifecyclePhase::Closed);
        assert!(runtime.lifecycle.access().removal_attempt().is_some());
        assert!(
            runtime
                .shutdown_deps()
                .has_addin_lifecycle(&access)
                .is_err()
        );
        assert!(runtime.begin_open().is_err());
        finalized.record_return();
        assert!(runtime.lifecycle.access().removal_attempt().is_none());
        assert!(runtime.begin_open().is_ok());
    }

    #[test]
    fn release_failure_retains_logical_close_empty_binding_and_removal_claim() {
        let runtime = Runtime::new();
        let pending = pending(&runtime);
        let access = runtime.bind_addin_lifecycle().unwrap();
        crate::runtime::capabilities::FAIL_BINDING_RELEASE.with(|fail| fail.set(true));
        let failure = match pending.finish() {
            Err(failure) => failure,
            Ok(_) => panic!("release fault"),
        };
        assert!(matches!(
            failure.recovery,
            FailedFinalization::Release { .. }
        ));
        assert_eq!(runtime.phase(), LifecyclePhase::Closed);
        assert!(runtime.lifecycle.access().removal_attempt().is_some());
        assert_eq!(
            runtime.shutdown_deps().has_addin_lifecycle(&access),
            Ok(false)
        );
        assert!(
            runtime
                .shutdown_deps()
                .empty_addin_lifecycle(&access)
                .is_err()
        );
        assert!(runtime.begin_open().is_err());
        drop(failure);
        assert_eq!(runtime.phase(), LifecyclePhase::Quarantined);
        assert!(runtime.lifecycle.access().removal_attempt().is_none());
    }

    fn assert_release_unwind<K: TerminalCertificateKind>(
        runtime: &Runtime<()>,
        pending: CompletionPendingBinding<'_, (), K>,
    ) {
        crate::runtime::capabilities::PANIC_BINDING_RELEASE.with(|fail| fail.set(true));
        let result = crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(|| {
            pending.finish()
        }));
        assert!(result.is_err());
        assert_eq!(runtime.phase(), LifecyclePhase::Quarantined);
        assert!(runtime.lifecycle.access().removal_attempt().is_none());
    }

    #[test]
    fn release_unwind_quarantines_atomically_for_both_purposes() {
        {
            let runtime = Runtime::new();
            assert_release_unwind(&runtime, pending(&runtime));
        }
        let runtime = Runtime::new();
        assert_release_unwind(&runtime, pending_rollback(&runtime));
    }

    fn assert_arm_unwind<K: TerminalCertificateKind>(
        runtime: &Runtime<()>,
        pending: CompletionPendingBinding<'_, (), K>,
    ) {
        PANIC_DURING_ARM.with(|fault| fault.set(true));
        let result = crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(|| {
            pending.finish()
        }));
        assert!(result.is_err());
        assert_eq!(runtime.phase(), LifecyclePhase::Quarantined);
        assert!(runtime.lifecycle.access().removal_attempt().is_none());
    }

    #[test]
    fn arm_unwind_uses_the_certified_recovery_for_both_purposes() {
        {
            let runtime = Runtime::new();
            assert_arm_unwind(&runtime, pending(&runtime));
        }
        let runtime = Runtime::new();
        assert_arm_unwind(&runtime, pending_rollback(&runtime));
    }

    #[test]
    fn open_rollback_keeps_the_claim_through_logical_close_and_binding_release() {
        let runtime = Runtime::new();
        let pending = pending_rollback(&runtime);
        let access = runtime.bind_addin_lifecycle().unwrap();
        let logical = pending
            .arm_finalization()
            .finish_logical()
            .unwrap_or_else(|_| panic!("logical finish"));
        assert_eq!(runtime.phase(), LifecyclePhase::Closed);
        assert!(runtime.lifecycle.access().removal_attempt().is_some());
        assert!(
            runtime
                .shutdown_deps()
                .empty_addin_lifecycle(&access)
                .is_err()
        );
        let released = logical
            .release_binding()
            .unwrap_or_else(|_| panic!("release"));
        assert!(
            runtime
                .shutdown_deps()
                .has_addin_lifecycle(&access)
                .is_err()
        );
        assert!(runtime.begin_open().is_err());
        let finalized = released.finalize().unwrap_or_else(|_| panic!("finalize"));
        drop(finalized);
        assert!(runtime.lifecycle.access().removal_attempt().is_none());
        assert!(runtime.begin_open().is_ok());
    }

    #[test]
    fn abandoned_certified_completion_quarantines_before_returning_claim() {
        let runtime = Runtime::new();
        drop(pending(&runtime));
        assert_eq!(runtime.phase(), LifecyclePhase::Quarantined);
        assert!(runtime.lifecycle.access().removal_attempt().is_none());
    }

    #[test]
    fn quarantine_requested_after_binding_release_prevents_finalized_success() {
        let runtime = Runtime::new();
        let pending = pending(&runtime);
        QUARANTINE_AFTER_BINDING_RELEASE.with(|request| request.set(true));
        let failure = match pending.finish() {
            Err(failure) => failure,
            Ok(_) => panic!("quarantine request must prevent boundary success"),
        };
        assert!(matches!(failure.recovery, FailedFinalization::Boundary(_)));
        // Logical close already committed, while the terminal failure still
        // owns its claim. No owner with a success operation has been issued.
        assert_eq!(runtime.phase(), LifecyclePhase::Closed);
        assert!(runtime.lifecycle.access().removal_attempt().is_some());
        assert!(runtime.begin_open().is_err());
        drop(failure);
        assert_eq!(runtime.phase(), LifecyclePhase::Quarantined);
        assert!(runtime.lifecycle.access().removal_attempt().is_none());
    }

    fn assert_stale_certificate<K: TerminalCertificateKind>(
        runtime: &Runtime<()>,
        pending: CompletionPendingBinding<'_, (), K>,
    ) {
        let phase = runtime.phase();
        runtime.return_protocol.reopen_admission().unwrap();
        runtime.return_protocol.close_admission();
        let failure = match pending.finish() {
            Err(failure) => failure,
            Ok(_) => panic!("stale return drain must not complete logical closure"),
        };
        assert!(matches!(failure.recovery, FailedFinalization::Armed(_)));
        assert_eq!(runtime.phase(), phase);
        assert!(runtime.lifecycle.access().removal_attempt().is_some());
        drop(failure);
        assert_eq!(runtime.phase(), LifecyclePhase::Quarantined);
        assert!(runtime.lifecycle.access().removal_attempt().is_none());
    }

    #[test]
    fn stale_certificate_retains_the_armed_stage_for_both_purposes() {
        {
            let runtime = Runtime::new();
            assert_stale_certificate(&runtime, pending(&runtime));
        }
        let runtime = Runtime::new();
        assert_stale_certificate(&runtime, pending_rollback(&runtime));
    }

    #[cfg(not(miri))]
    #[test]
    #[allow(
        clippy::mem_forget,
        reason = "adversarial forgotten completion must retain its claim"
    )]
    fn forgotten_completion_cannot_publish_boundary_success_or_release_claim() {
        let runtime = Runtime::new();
        std::mem::forget(pending(&runtime));
        assert_eq!(runtime.phase(), LifecyclePhase::Closing);
        assert!(runtime.lifecycle.access().removal_attempt().is_some());
        assert!(runtime.begin_open().is_err());
    }
}
