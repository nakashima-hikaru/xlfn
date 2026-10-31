//! Boundary completion owns both logical completion and the exact binding.
//! Dropping/forgetting intermediate work never issues a finalized result.
use super::certificate::{FinalRemoval, OpenRollback, TerminalCertificateKind};
use crate::runtime::capabilities::{EmptyLifecycle, LifecycleFailure, ShutdownDeps};
use crate::{Addin, XllError};

#[must_use = "finish logical closure and release the retained binding"]
pub(crate) struct CompletionPendingBinding<'runtime, A: Addin, K: TerminalCertificateKind> {
    deps: ShutdownDeps<'runtime, A>,
    finished: bool,
    state: Option<CompletionState<'runtime, A, K>>,
}

#[allow(
    dead_code,
    reason = "terminal failures retain progressed ownership until quarantine"
)]
enum CompletionState<'runtime, A: Addin, K: TerminalCertificateKind> {
    Certified {
        certificate: K::Certificate<'runtime, A>,
        empty: EmptyLifecycle<'runtime, A>,
    },
    Logical {
        completed: K::Completed<'runtime, A>,
        failure: LifecycleFailure<'runtime, A>,
    },
}

#[must_use = "the failure retains the progressed completion and binding responsibilities"]
pub(crate) struct FinalizationFailure<'runtime, A: Addin, K: TerminalCertificateKind> {
    error: XllError,
    _pending: CompletionPendingBinding<'runtime, A, K>,
}
impl<A: Addin, K: TerminalCertificateKind> FinalizationFailure<'_, A, K> {
    pub(crate) fn error(&self) -> &XllError {
        &self.error
    }
    pub(crate) fn quarantine(self: Box<Self>) -> XllError {
        let error = self.error().clone();
        drop(self);
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
            deps,
            finished: false,
            state: Some(CompletionState::Certified { certificate, empty }),
        }
    }
    pub(crate) fn finish(
        mut self,
    ) -> Result<Finalized<'runtime, A, K>, Box<FinalizationFailure<'runtime, A, K>>> {
        let Some(CompletionState::Certified {
            mut certificate,
            empty,
        }) = self.state.take()
        else {
            xlfn_kernel::invariant::fail_stop();
        };
        // The claim quarantines atomically on any error or unwind until the
        // exact empty binding has been released successfully.
        K::arm_finalization(&mut certificate);
        let mut completed = match K::finish(certificate) {
            Ok(completed) => completed,
            Err((error, certificate)) => {
                self.state = Some(CompletionState::Certified {
                    certificate: *certificate,
                    empty,
                });
                return Err(Box::new(FinalizationFailure {
                    error,
                    _pending: self,
                }));
            }
        };
        // The removal claim in completed remains live through this last step.
        match empty.release() {
            Ok(()) => {
                K::complete_finalization(&mut completed);
                self.finished = true;
                Ok(Finalized {
                    deps: self.deps,
                    completed,
                })
            }
            Err(failure) => {
                let error = failure.error().clone();
                self.state = Some(CompletionState::Logical { completed, failure });
                Err(Box::new(FinalizationFailure {
                    error,
                    _pending: self,
                }))
            }
        }
    }
}
impl<A: Addin, K: TerminalCertificateKind> Drop for CompletionPendingBinding<'_, A, K> {
    fn drop(&mut self) {
        if !self.finished {
            match self.state.as_mut() {
                Some(CompletionState::Logical { completed, .. }) => {
                    // The owning claim atomically publishes terminal failure as
                    // it returns; Closed/Finalizing cannot be quarantined alone.
                    K::mark_finalization_failed(completed);
                }
                _ => self.deps.quarantine_state(),
            }
        }
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
    use super::*;
    use crate::lifecycle::LifecyclePhase;
    use crate::runtime::Runtime;

    fn pending(runtime: &Runtime<()>) -> CompletionPendingBinding<'_, (), FinalRemoval> {
        let opening = runtime.begin_open().unwrap();
        let mut opening = runtime.publish(opening, (), ());
        runtime.finish_open(&mut opening, Vec::new()).unwrap();
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
            .unwrap_or_else(|_| panic!("quiesce"))
            .seal_services()
            .unwrap_or_else(|_| panic!("seal"))
            .cleanup_addin(&lifecycle, &mut crate::shutdown::CloseReport::default())
            .unwrap_or_else(|_| panic!("cleanup"))
            .finish_services()
            .unwrap_or_else(|_| panic!("services"))
            .reclaim(|_| {})
            .unwrap_or_else(|_| panic!("reclaim"));
        teardown.certify().unwrap_or_else(|_| panic!("certify"))
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
            failure._pending.state,
            Some(CompletionState::Logical { .. })
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

    #[test]
    fn release_unwind_quarantines_atomically_with_claim_return() {
        let runtime = Runtime::new();
        let pending = pending(&runtime);
        crate::runtime::capabilities::PANIC_BINDING_RELEASE.with(|fail| fail.set(true));
        let result = crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(|| {
            pending.finish()
        }));
        assert!(result.is_err());
        assert_eq!(runtime.phase(), LifecyclePhase::Quarantined);
        assert!(runtime.lifecycle.access().removal_attempt().is_none());
    }

    #[test]
    fn abandoned_certified_completion_quarantines_before_returning_claim() {
        let runtime = Runtime::new();
        drop(pending(&runtime));
        assert_eq!(runtime.phase(), LifecyclePhase::Quarantined);
        assert!(runtime.lifecycle.access().removal_attempt().is_none());
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
