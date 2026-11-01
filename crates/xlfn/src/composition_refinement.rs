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
    returned_success: bool,
    quarantined: bool,
    // Observation-only progress. These flags never issue a cleanup authority.
    cleanup_owner_present: bool,
    quarantine_pending: bool,
    // A later failure observation must not erase an already observed unsafe
    // success. Only a new attempt may reset this diagnostic history.
    success_after_quarantine: bool,
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
            cleanup_owner_present: false,
            quarantine_pending: false,
            success_after_quarantine: false,
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
                machine.success_after_quarantine = false;
            }
            CompositionEvent::QuarantineOpen { .. } => {
                machine.quarantined = true;
                machine.returned_success = false;
                machine.terminal_pending = false;
            }
            CompositionEvent::AcquireFinalCloseOwner
            | CompositionEvent::AcquireOpenRollbackOwner => {
                machine.cleanup_owner_present = true;
            }
            CompositionEvent::LiftShutdown(CertificateEvent::Quarantine(_)) => {
                machine.quarantine_pending = machine.cleanup_owner_present;
                machine.quarantined = !machine.cleanup_owner_present;
                machine.returned_success = false;
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

    pub(crate) fn finish_return(&self) {
        let mut machine = self.inner.lock();
        if machine.return_pending || machine.terminal_pending {
            if machine.return_pending && (machine.quarantined || machine.quarantine_pending) {
                machine.success_after_quarantine = true;
            }
            machine.returned_success = machine.return_pending;
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
            // Never hide an observed success behind an earlier failure flag:
            // the formal checker must reject success after quarantine.
            outcome: if machine.success_after_quarantine || machine.returned_success {
                "returned_success"
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
}
