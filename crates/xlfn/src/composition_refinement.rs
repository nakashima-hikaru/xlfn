use crate::shutdown_trace::{CertificateEvent, ShutdownFailure, ShutdownResources};
use crate::sync::Mutex;
use serde::Serialize;

const MAX_TRACE_EVENTS: usize = 16_384;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum CompositionEvent {
    BeginOpen {
        #[serde(rename = "sampledEpoch")]
        sampled_epoch: u64,
        attempt: u64,
    },
    FinishOpenRejectedByClose {
        attempt: u64,
    },
    FailOpen {
        attempt: u64,
    },
    QuarantineOpen {
        attempt: u64,
        reason: ShutdownFailure,
    },
    RequestFinalClose,
    AcquireFinalCloseOwner,
    AcquireOpenRollbackOwner,
    CommitOpen {
        attempt: u64,
        resources: ShutdownResources,
    },
    LiftShutdown(CertificateEvent),
    FinishCommittedShutdown,
    PublishCommittedClosed,
    RetireCommittedShutdown,
    FinishUncommittedFinalClose(ShutdownResources),
    FinishOpenRollback(ShutdownResources),
    ReleaseCleanupOwner,
}

#[cfg(test)]
#[derive(Serialize)]
struct TraceDocument {
    initial: &'static str,
    events: Vec<CompositionEvent>,
    #[serde(rename = "trace_truncated")]
    trace_truncated: bool,
    outcome: &'static str,
}

struct Machine {
    // Composition history belongs to the Runtime lifetime, not to one
    // explicit removal transaction. A later beginOpen therefore replays after the
    // closeEpoch and generation transitions emitted by earlier cycles.
    events: Vec<CompositionEvent>,
    trace_truncated: bool,
    // A failure before or after an observed success must not hide that
    // return from the checker. Only a new attempt resets this history.
    returned_success: bool,
    quarantined: bool,
    fail_stopped: bool,
    // Observation-only progress. These flags never issue a cleanup authority.
    cleanup_owner_present: bool,
    quarantine_pending: bool,
    return_pending: bool,
    terminal_pending: bool,
}

impl Machine {
    const fn new() -> Self {
        Self {
            events: Vec::new(),
            trace_truncated: false,
            returned_success: false,
            quarantined: false,
            fail_stopped: false,
            cleanup_owner_present: false,
            quarantine_pending: false,
            return_pending: false,
            terminal_pending: false,
        }
    }

    fn push(&mut self, event: CompositionEvent) {
        if self.events.len() < MAX_TRACE_EVENTS {
            self.events.push(event);
        } else {
            self.trace_truncated = true;
        }
    }
}

pub(crate) struct CompositionTrace {
    inner: Mutex<Machine>,
}

impl CompositionTrace {
    pub(crate) const fn new() -> Self {
        Self {
            inner: Mutex::new(Machine::new()),
        }
    }

    pub(crate) fn record(&self, event: CompositionEvent) {
        let mut machine = self.inner.lock();
        match &event {
            CompositionEvent::BeginOpen { .. } => {
                machine.returned_success = false;
                machine.return_pending = false;
                machine.terminal_pending = false;
                machine.cleanup_owner_present = false;
                machine.quarantine_pending = false;
            }
            CompositionEvent::QuarantineOpen { .. } => {
                machine.quarantined = true;
                machine.terminal_pending = false;
            }
            CompositionEvent::AcquireFinalCloseOwner
            | CompositionEvent::AcquireOpenRollbackOwner => {
                machine.cleanup_owner_present = true;
            }
            CompositionEvent::LiftShutdown(CertificateEvent::Quarantine(_)) => {
                machine.quarantine_pending = machine.cleanup_owner_present;
                machine.quarantined = !machine.cleanup_owner_present;
                machine.terminal_pending = false;
            }
            CompositionEvent::LiftShutdown(CertificateEvent::FailStop(_)) => {
                // Fail-stop is terminal even with an active cleanup owner:
                // unlike retention quarantine, it never waits for release.
                machine.fail_stopped = true;
                machine.terminal_pending = false;
            }
            CompositionEvent::ReleaseCleanupOwner => {
                machine.cleanup_owner_present = false;
                if machine.quarantine_pending {
                    machine.quarantined = true;
                    machine.quarantine_pending = false;
                }
            }
            _ => {}
        }
        machine.push(event);
    }

    pub(crate) fn mark_return_pending(&self) {
        let mut machine = self.inner.lock();
        machine.return_pending = true;
    }

    pub(crate) fn mark_truncated(&self) {
        self.inner.lock().trace_truncated = true;
    }

    pub(crate) fn finish_return(&self) {
        let mut machine = self.inner.lock();
        if machine.return_pending || machine.terminal_pending {
            machine.returned_success |= machine.return_pending;
            machine.return_pending = false;
            machine.terminal_pending = false;
        }
    }

    pub(crate) fn mark_terminal_pending(&self) {
        let mut machine = self.inner.lock();
        machine.terminal_pending = true;
    }

    #[cfg(test)]
    pub(crate) fn trace_json(&self) -> Result<String, serde_json::Error> {
        let machine = self.inner.lock();
        let document = TraceDocument {
            initial: "initial",
            events: machine.events.clone(),
            trace_truncated: machine.trace_truncated,
            // Never hide an observed success behind a failure flag, regardless
            // of their order: the checker must inspect the final state.
            outcome: if machine.returned_success {
                "returned_success"
            } else if machine.fail_stopped {
                "fail_stopped"
            } else if machine.quarantined {
                "quarantined"
            } else {
                "in_progress"
            },
        };
        serde_json::to_string_pretty(&document)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(trace: &CompositionTrace) -> String {
        let document: serde_json::Value =
            serde_json::from_str(&trace.trace_json().unwrap()).unwrap();
        document["outcome"].as_str().unwrap().to_owned()
    }

    #[test]
    fn owned_quarantine_stays_in_progress_until_the_observed_owner_releases() {
        let trace = CompositionTrace::new();
        trace.record(CompositionEvent::AcquireFinalCloseOwner);
        trace.record(CompositionEvent::LiftShutdown(
            CertificateEvent::Quarantine(ShutdownFailure::AddinShutdownFailed),
        ));
        assert_eq!(outcome(&trace), "in_progress");
        trace.record(CompositionEvent::ReleaseCleanupOwner);
        assert_eq!(outcome(&trace), "quarantined");
    }

    #[test]
    fn success_observed_after_quarantine_is_not_hidden_from_the_checker() {
        let trace = CompositionTrace::new();
        trace.record(CompositionEvent::QuarantineOpen {
            attempt: 1,
            reason: ShutdownFailure::AddinShutdownFailed,
        });
        trace.mark_return_pending();
        trace.finish_return();
        assert_eq!(outcome(&trace), "returned_success");
        trace.record(CompositionEvent::QuarantineOpen {
            attempt: 1,
            reason: ShutdownFailure::AddinShutdownFailed,
        });
        assert_eq!(outcome(&trace), "returned_success");
        trace.record(CompositionEvent::LiftShutdown(
            CertificateEvent::Quarantine(ShutdownFailure::AddinShutdownFailed),
        ));
        assert_eq!(outcome(&trace), "returned_success");
    }

    #[test]
    fn quarantine_cannot_cancel_a_pending_success_observation() {
        for owned in [false, true] {
            let trace = CompositionTrace::new();
            if owned {
                trace.record(CompositionEvent::AcquireFinalCloseOwner);
            }
            trace.mark_return_pending();
            trace.record(CompositionEvent::LiftShutdown(
                CertificateEvent::Quarantine(ShutdownFailure::AddinShutdownFailed),
            ));
            trace.finish_return();
            assert_eq!(outcome(&trace), "returned_success");
        }
        let trace = CompositionTrace::new();
        trace.mark_return_pending();
        trace.record(CompositionEvent::QuarantineOpen {
            attempt: 1,
            reason: ShutdownFailure::AddinShutdownFailed,
        });
        trace.finish_return();
        assert_eq!(outcome(&trace), "returned_success");
    }

    #[test]
    fn fail_stop_is_terminal_without_releasing_the_cleanup_owner() {
        for owned in [false, true] {
            let trace = CompositionTrace::new();
            if owned {
                trace.record(CompositionEvent::AcquireFinalCloseOwner);
            }
            trace.record(CompositionEvent::LiftShutdown(CertificateEvent::FailStop(
                ShutdownFailure::InvariantViolation,
            )));
            assert_eq!(outcome(&trace), "fail_stopped");
            trace.mark_terminal_pending();
            trace.finish_return();
            assert_eq!(outcome(&trace), "fail_stopped");
        }
    }

    #[test]
    fn fail_stop_does_not_hide_a_pending_or_subsequent_success() {
        for pending in [false, true] {
            let trace = CompositionTrace::new();
            if pending {
                trace.mark_return_pending();
            }
            trace.record(CompositionEvent::LiftShutdown(CertificateEvent::FailStop(
                ShutdownFailure::InvariantViolation,
            )));
            if !pending {
                trace.mark_return_pending();
            }
            trace.finish_return();
            assert_eq!(outcome(&trace), "returned_success");
            trace.record(CompositionEvent::LiftShutdown(CertificateEvent::FailStop(
                ShutdownFailure::BoundaryPanic,
            )));
            assert_eq!(outcome(&trace), "returned_success");
        }
    }

    #[test]
    fn success_observed_before_failure_is_not_hidden_from_the_checker() {
        for failure in [
            CompositionEvent::QuarantineOpen {
                attempt: 1,
                reason: ShutdownFailure::AddinShutdownFailed,
            },
            CompositionEvent::LiftShutdown(CertificateEvent::Quarantine(
                ShutdownFailure::AddinShutdownFailed,
            )),
            CompositionEvent::LiftShutdown(CertificateEvent::FailStop(
                ShutdownFailure::InvariantViolation,
            )),
        ] {
            let trace = CompositionTrace::new();
            trace.mark_return_pending();
            trace.finish_return();
            trace.record(failure);
            assert_eq!(outcome(&trace), "returned_success");
            trace.mark_terminal_pending();
            trace.finish_return();
            assert_eq!(outcome(&trace), "returned_success");
        }
    }

    #[test]
    fn a_new_open_attempt_resets_the_previous_success_observation() {
        for (failure, expected) in [
            (
                CompositionEvent::QuarantineOpen {
                    attempt: 2,
                    reason: ShutdownFailure::AddinShutdownFailed,
                },
                "quarantined",
            ),
            (
                CompositionEvent::LiftShutdown(CertificateEvent::Quarantine(
                    ShutdownFailure::AddinShutdownFailed,
                )),
                "quarantined",
            ),
            (
                CompositionEvent::LiftShutdown(CertificateEvent::FailStop(
                    ShutdownFailure::InvariantViolation,
                )),
                "fail_stopped",
            ),
        ] {
            let trace = CompositionTrace::new();
            trace.mark_return_pending();
            trace.finish_return();
            trace.record(CompositionEvent::BeginOpen {
                sampled_epoch: 1,
                attempt: 2,
            });
            assert_eq!(outcome(&trace), "in_progress");
            trace.record(failure);
            assert_eq!(outcome(&trace), expected);
        }
    }
}
