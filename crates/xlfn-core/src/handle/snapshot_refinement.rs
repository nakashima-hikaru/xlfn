use parking_lot::Mutex;
use serde::Serialize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[cfg(test)]
pub(crate) const SCHEMA_VERSION: u32 = 1;
const MAX_TRACE_EVENTS: usize = 16_384;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SnapshotTokenWire {
    pub(crate) session: u64,
    pub(crate) slot: u64,
    pub(crate) generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "event", rename_all = "camelCase")]
pub(crate) enum Event {
    InsertFresh,
    InsertReuse {
        slot: u64,
        generation: u64,
    },
    RemoveReuse {
        token: SnapshotTokenWire,
        #[serde(rename = "nextGeneration")]
        next_generation: u64,
    },
    RemoveRetire {
        token: SnapshotTokenWire,
    },
    BeginFastObservation {
        #[serde(rename = "readerId")]
        reader_id: u64,
        token: SnapshotTokenWire,
    },
    AcquireTentativeLease {
        #[serde(rename = "readerId")]
        reader_id: u64,
    },
    AbandonObservation {
        #[serde(rename = "readerId")]
        reader_id: u64,
    },
    ValidateFastLookup {
        #[serde(rename = "readerId")]
        reader_id: u64,
    },
    RejectTentativeFastLookup {
        #[serde(rename = "readerId")]
        reader_id: u64,
    },
    CompleteFastLookup {
        #[serde(rename = "readerId")]
        reader_id: u64,
    },
    FallbackFastLookup {
        #[serde(rename = "readerId")]
        reader_id: u64,
    },
    BeginSlowLookup {
        token: SnapshotTokenWire,
    },
    EndSlowLookup,
    BeginSealLeaseAdmission,
    FinishSealLeaseAdmission,
    CloseRegistry,
    FinishClose,
}

#[cfg(test)]
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct InitialWire {
    pub(crate) session: u64,
}

#[cfg(test)]
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct TraceWire {
    pub(crate) schema_version: u32,
    pub(crate) initial: InitialWire,
    pub(crate) events: Vec<Event>,
    pub(crate) trace_truncated: bool,
    pub(crate) outcome: String,
}

pub(crate) struct SnapshotTraceRecorder {
    #[cfg(test)]
    session: u64,
    events: Mutex<Vec<Event>>,
    next_reader_id: AtomicU64,
    truncated: AtomicBool,
}

impl SnapshotTraceRecorder {
    pub(crate) fn new(session: u64) -> Self {
        #[cfg(not(test))]
        let _ = session;
        Self {
            #[cfg(test)]
            session,
            events: Mutex::new(Vec::new()),
            next_reader_id: AtomicU64::new(1),
            truncated: AtomicBool::new(false),
        }
    }

    pub(crate) fn next_reader_id(&self) -> u64 {
        self.next_reader_id.fetch_add(1, Ordering::Relaxed)
    }

    pub(crate) fn record(&self, event: Event) {
        let mut events = self.events.lock();
        if events.len() < MAX_TRACE_EVENTS {
            events.push(event);
        } else {
            self.truncated.store(true, Ordering::Relaxed);
        }
    }

    #[cfg(test)]
    pub(crate) fn export_json(&self, outcome: &str) -> String {
        let events = self.events.lock().clone();
        let trace = TraceWire {
            schema_version: SCHEMA_VERSION,
            initial: InitialWire {
                session: self.session,
            },
            events,
            trace_truncated: self.truncated.load(Ordering::Relaxed),
            outcome: outcome.to_string(),
        };
        serde_json::to_string(&trace).expect("Snapshot trace must serialize to JSON")
    }
}

pub(crate) enum LineageKind {
    Fast { reader_id: u64 },
    Slow,
}

pub(crate) struct LeaseLineageTrace {
    recorder: Arc<SnapshotTraceRecorder>,
    kind: LineageKind,
}

impl LeaseLineageTrace {
    pub(crate) fn new_fast(recorder: Arc<SnapshotTraceRecorder>, reader_id: u64) -> Arc<Self> {
        Arc::new(Self {
            recorder,
            kind: LineageKind::Fast { reader_id },
        })
    }

    pub(crate) fn new_slow(recorder: Arc<SnapshotTraceRecorder>) -> Arc<Self> {
        Arc::new(Self {
            recorder,
            kind: LineageKind::Slow,
        })
    }
}

impl Drop for LeaseLineageTrace {
    fn drop(&mut self) {
        match self.kind {
            LineageKind::Fast { reader_id } => {
                self.recorder
                    .record(Event::CompleteFastLookup { reader_id });
            }
            LineageKind::Slow => {
                self.recorder.record(Event::EndSlowLookup);
            }
        }
    }
}
