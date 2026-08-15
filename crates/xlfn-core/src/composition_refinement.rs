use crate::shutdown_refinement::{GhostEvent, GhostResources};
use parking_lot::Mutex;
use serde::Serialize;

#[cfg(test)]
pub(crate) const SCHEMA_VERSION: u32 = 1;
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
    RequestFinalClose,
    AcquireFinalCloseOwner,
    AcquireOpenRollbackOwner,
    CommitOpen {
        attempt: u64,
        resources: GhostResources,
    },
    LiftShutdown(GhostEvent),
    FinishCommittedShutdown,
    PublishCommittedClosed,
    RetireCommittedShutdown,
    FinishUncommittedFinalClose(GhostResources),
    FinishOpenRollback(GhostResources),
    ReleaseCleanupOwner,
}

#[cfg(test)]
#[derive(Serialize)]
struct TraceDocument {
    #[serde(rename = "schema_version")]
    schema_version: u32,
    initial: &'static str,
    events: Vec<CompositionEvent>,
    #[serde(rename = "trace_truncated")]
    trace_truncated: bool,
    outcome: &'static str,
}

struct Machine {
    // Composition history belongs to the Runtime lifetime, not to one
    // xlAutoClose transaction. A later beginOpen therefore replays after the
    // closeEpoch and generation transitions emitted by earlier cycles.
    events: Vec<CompositionEvent>,
    trace_truncated: bool,
    returned_success: bool,
    return_pending: bool,
    terminal_pending: bool,
}

impl Machine {
    const fn new() -> Self {
        Self {
            events: Vec::new(),
            trace_truncated: false,
            returned_success: false,
            return_pending: false,
            terminal_pending: false,
        }
    }

    fn begin_open(&mut self, sampled_epoch: u64, attempt: u64) {
        self.returned_success = false;
        self.return_pending = false;
        self.terminal_pending = false;
        self.push(CompositionEvent::BeginOpen {
            sampled_epoch,
            attempt,
        });
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

    pub(crate) fn begin_open(&self, sampled_epoch: u64, attempt: u64) {
        let mut machine = self.inner.lock();
        machine.begin_open(sampled_epoch, attempt);
    }

    pub(crate) fn record(&self, event: CompositionEvent) {
        self.inner.lock().push(event);
    }

    pub(crate) fn mark_return_pending(&self) {
        let mut machine = self.inner.lock();
        machine.return_pending = true;
    }

    pub(crate) fn finish_return(&self) {
        let mut machine = self.inner.lock();
        if machine.return_pending || machine.terminal_pending {
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
    pub(crate) fn events(&self) -> Vec<CompositionEvent> {
        self.inner.lock().events.clone()
    }

    #[cfg(test)]
    pub(crate) fn trace_json(&self) -> Result<String, serde_json::Error> {
        let machine = self.inner.lock();
        let document = TraceDocument {
            schema_version: SCHEMA_VERSION,
            initial: "initial",
            events: machine.events.clone(),
            trace_truncated: machine.trace_truncated,
            outcome: if machine.returned_success {
                "returned_success"
            } else {
                "in_progress"
            },
        };
        serde_json::to_string_pretty(&document)
    }
}
