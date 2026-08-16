#![allow(
    dead_code,
    reason = "H4 handle traces are consumed by the feature-gated Lean checker"
)]

use super::{FormulaRevisionKey, HandleTopicKey, HandleTopicOwner};
use parking_lot::{Mutex, MutexGuard};
use serde::Serialize;
use std::collections::HashMap;

pub(crate) const SCHEMA_VERSION: u32 = 3;
const MAX_TRACE_EVENTS: usize = 16_384;

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FormulaRevisionKeyWire {
    pub(crate) sheet_id: u64,
    pub(crate) row: i32,
    pub(crate) column: i32,
    pub(crate) udf_id: String,
    pub(crate) input_fingerprint: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TokenWire {
    pub(crate) session: u64,
    pub(crate) slot: u64,
    pub(crate) generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OwnerWire {
    pub(crate) server_generation: u64,
    pub(crate) topic_id: i32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "event", rename_all = "camelCase")]
pub(crate) enum Event {
    BeginPrepare,
    EndPrepare,
    BeginInitializer {
        key: FormulaRevisionKeyWire,
        #[serde(rename = "runtimeId")]
        runtime_id: u64,
    },
    FinishInitializer {
        key: FormulaRevisionKeyWire,
        #[serde(rename = "runtimeId")]
        runtime_id: u64,
    },
    InsertPendingFresh {
        key: FormulaRevisionKeyWire,
        #[serde(rename = "runtimeId")]
        runtime_id: u64,
    },
    InsertPendingReuse {
        key: FormulaRevisionKeyWire,
        #[serde(rename = "runtimeId")]
        runtime_id: u64,
        slot: u64,
        generation: u64,
    },
    PublishAndInstallProvisional {
        key: FormulaRevisionKeyWire,
        #[serde(rename = "runtimeId")]
        runtime_id: u64,
        token: TokenWire,
        #[serde(rename = "rtdKey")]
        rtd_key: String,
    },
    CommitAndActivate {
        key: FormulaRevisionKeyWire,
        #[serde(rename = "runtimeId")]
        runtime_id: u64,
        token: TokenWire,
    },
    WithdrawAndInvalidate {
        key: FormulaRevisionKeyWire,
        #[serde(rename = "runtimeId")]
        runtime_id: u64,
        token: TokenWire,
    },
    RollbackPendingReuse {
        key: FormulaRevisionKeyWire,
        #[serde(rename = "runtimeId")]
        runtime_id: u64,
        #[serde(rename = "nextGeneration")]
        next_generation: u64,
    },
    RollbackPendingRetire {
        key: FormulaRevisionKeyWire,
        #[serde(rename = "runtimeId")]
        runtime_id: u64,
    },
    BeginWarmRead {
        #[serde(rename = "readerId")]
        reader_id: u64,
        key: FormulaRevisionKeyWire,
    },
    FinishWarmRead {
        #[serde(rename = "readerId")]
        reader_id: u64,
    },
    FailWarmRead {
        #[serde(rename = "readerId")]
        reader_id: u64,
    },
    AbandonWarmRead {
        #[serde(rename = "readerId")]
        reader_id: u64,
    },
    ClaimServer {
        key: FormulaRevisionKeyWire,
        generation: u64,
    },
    BeginConnection {
        key: FormulaRevisionKeyWire,
        owner: OwnerWire,
    },
    ReuseCommittedConnection {
        key: FormulaRevisionKeyWire,
        owner: OwnerWire,
    },
    CommitConnection {
        key: FormulaRevisionKeyWire,
        owner: OwnerWire,
    },
    RollbackConnection {
        key: FormulaRevisionKeyWire,
        owner: OwnerWire,
    },
    Disconnect {
        key: FormulaRevisionKeyWire,
        owner: OwnerWire,
    },
    DetachGeneration {
        generation: u64,
    },
    DrainPendingReuse {
        token: TokenWire,
        #[serde(rename = "runtimeId")]
        runtime_id: u64,
        #[serde(rename = "nextGeneration")]
        next_generation: u64,
    },
    DrainPendingRetire {
        token: TokenWire,
        #[serde(rename = "runtimeId")]
        runtime_id: u64,
    },
    DrainPublishedReuse {
        token: TokenWire,
        #[serde(rename = "nextGeneration")]
        next_generation: u64,
    },
    DrainPublishedRetire {
        token: TokenWire,
    },
    SealForClose,
    CloseRegistry,
    FinishClose,
}

#[derive(Serialize)]
struct InitialWire {
    session: u64,
}

#[derive(Serialize)]
struct TraceDocument {
    schema_version: u32,
    initial: InitialWire,
    events: Vec<Event>,
    trace_truncated: bool,
    outcome: &'static str,
}

struct Machine {
    session: u64,
    events: Vec<Event>,
    trace_truncated: bool,
    next_initializer_id: u64,
    next_reader_id: u64,
    returned_success: bool,
    initializers: HashMap<FormulaRevisionKeyWire, u64>,
    #[cfg(test)]
    before_seal_hook: Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>,
}

impl Machine {
    fn new(session: u64) -> Self {
        Self {
            session,
            events: Vec::new(),
            trace_truncated: false,
            next_initializer_id: 1,
            next_reader_id: 1,
            returned_success: false,
            initializers: HashMap::new(),
            #[cfg(test)]
            before_seal_hook: None,
        }
    }

    fn push(&mut self, event: Event) {
        if self.events.len() < MAX_TRACE_EVENTS {
            self.events.push(event);
        } else {
            self.trace_truncated = true;
        }
    }
}

pub(crate) struct PrepareGuard<'a> {
    trace: &'a HandleRefinementTrace,
}

impl Drop for PrepareGuard<'_> {
    fn drop(&mut self) {
        self.trace.end_prepare();
    }
}

/// Trace-only serialization domain for publication state and lifecycle events.
///
/// Production builds do not construct this guard. Test and trace builds use it
/// to make an atomic publication state read/store and its corresponding event
/// one observable linearization point.
pub(crate) struct Linearization<'a> {
    machine: MutexGuard<'a, Machine>,
}

impl Linearization<'_> {
    pub(crate) fn finish_initializer(&mut self, runtime_id: u64) {
        let key = self
            .machine
            .initializers
            .iter()
            .find_map(|(key, id)| (*id == runtime_id).then_some(key.clone()));
        self.machine.initializers.retain(|_, id| *id != runtime_id);
        if let Some(key) = key {
            self.machine
                .push(Event::FinishInitializer { key, runtime_id });
        }
    }

    pub(crate) fn commit_and_activate(
        &mut self,
        key: &HandleTopicKey,
        runtime_id: u64,
        token: TokenWire,
    ) {
        self.machine.push(Event::CommitAndActivate {
            key: topic_key(key),
            runtime_id,
            token,
        });
    }

    pub(crate) fn begin_warm_read(&mut self, key: &HandleTopicKey) -> u64 {
        let reader_id = self.machine.next_reader_id;
        self.machine.next_reader_id = reader_id.saturating_add(1);
        self.machine.push(Event::BeginWarmRead {
            reader_id,
            key: topic_key(key),
        });
        reader_id
    }

    pub(crate) fn finish_warm_read(&mut self, reader_id: u64) {
        self.machine.push(Event::FinishWarmRead { reader_id });
    }

    pub(crate) fn fail_warm_read(&mut self, reader_id: u64) {
        self.machine.push(Event::FailWarmRead { reader_id });
    }

    pub(crate) fn abandon_warm_read(&mut self, reader_id: u64) {
        self.machine.push(Event::AbandonWarmRead { reader_id });
    }

    pub(crate) fn withdraw_and_invalidate(
        &mut self,
        key: &HandleTopicKey,
        runtime_id: u64,
        token: TokenWire,
    ) {
        self.machine.push(Event::WithdrawAndInvalidate {
            key: topic_key(key),
            runtime_id,
            token,
        });
    }

    pub(crate) fn disconnect(&mut self, key: &HandleTopicKey, owner: HandleTopicOwner) {
        self.machine.push(Event::Disconnect {
            key: topic_key(key),
            owner: owner_wire(owner),
        });
    }

    pub(crate) fn detach_generation(&mut self, generation: u64) {
        self.machine.push(Event::DetachGeneration { generation });
    }

    pub(crate) fn seal_for_close(&mut self) {
        #[cfg(test)]
        if let Some((entered, release)) = self.machine.before_seal_hook.take() {
            entered.send(()).expect("H4 seal test hook receiver");
            release.recv().expect("H4 seal test hook release");
        }
        self.machine.push(Event::SealForClose);
    }
}

pub(crate) struct HandleRefinementTrace {
    inner: Mutex<Machine>,
}

impl HandleRefinementTrace {
    pub(crate) fn new(session: u64) -> Self {
        Self {
            inner: Mutex::new(Machine::new(session)),
        }
    }

    pub(crate) fn prepare_guard(&self) -> PrepareGuard<'_> {
        PrepareGuard { trace: self }
    }

    pub(crate) fn linearize(&self) -> Linearization<'_> {
        Linearization {
            machine: self.inner.lock(),
        }
    }

    #[cfg(test)]
    pub(crate) fn set_before_seal_hook(
        &self,
        entered: std::sync::mpsc::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    ) {
        self.inner.lock().before_seal_hook = Some((entered, release));
    }

    pub(crate) fn begin_prepare(&self) {
        self.inner.lock().push(Event::BeginPrepare);
    }

    fn end_prepare(&self) {
        self.inner.lock().push(Event::EndPrepare);
    }

    pub(crate) fn allocate_initializer_id(&self) -> u64 {
        let mut machine = self.inner.lock();
        let id = machine.next_initializer_id;
        machine.next_initializer_id = id.saturating_add(1);
        id
    }

    pub(crate) fn begin_initializer(&self, key: &HandleTopicKey, runtime_id: u64) {
        let key = topic_key(key);
        let mut machine = self.inner.lock();
        machine.initializers.insert(key.clone(), runtime_id);
        machine.push(Event::BeginInitializer { key, runtime_id });
    }

    pub(crate) fn finish_initializer(&self, runtime_id: u64) {
        self.linearize().finish_initializer(runtime_id);
    }

    pub(crate) fn insert_pending_fresh(&self, key: &HandleTopicKey, runtime_id: u64) {
        self.inner.lock().push(Event::InsertPendingFresh {
            key: topic_key(key),
            runtime_id,
        });
    }

    pub(crate) fn insert_pending_reuse(
        &self,
        key: &HandleTopicKey,
        runtime_id: u64,
        slot: u64,
        generation: u64,
    ) {
        self.inner.lock().push(Event::InsertPendingReuse {
            key: topic_key(key),
            runtime_id,
            slot,
            generation,
        });
    }

    pub(crate) fn publish_and_install(
        &self,
        key: &HandleTopicKey,
        runtime_id: u64,
        token: TokenWire,
        rtd_key: &str,
    ) {
        self.inner.lock().push(Event::PublishAndInstallProvisional {
            key: topic_key(key),
            runtime_id,
            token,
            rtd_key: rtd_key.to_owned(),
        });
    }

    pub(crate) fn commit_and_activate(
        &self,
        key: &HandleTopicKey,
        runtime_id: u64,
        token: TokenWire,
    ) {
        self.linearize().commit_and_activate(key, runtime_id, token);
    }

    pub(crate) fn withdraw_and_invalidate(
        &self,
        key: &HandleTopicKey,
        runtime_id: u64,
        token: TokenWire,
    ) {
        self.inner.lock().push(Event::WithdrawAndInvalidate {
            key: topic_key(key),
            runtime_id,
            token,
        });
    }

    pub(crate) fn rollback_pending(
        &self,
        key: &HandleTopicKey,
        runtime_id: u64,
        reusable: bool,
        token: TokenWire,
    ) {
        let mut machine = self.inner.lock();
        if reusable {
            machine.push(Event::RollbackPendingReuse {
                key: topic_key(key),
                runtime_id,
                next_generation: token.generation.saturating_add(1),
            });
        } else {
            machine.push(Event::RollbackPendingRetire {
                key: topic_key(key),
                runtime_id,
            });
        }
    }

    pub(crate) fn begin_warm_read(&self, key: &HandleTopicKey) -> u64 {
        self.linearize().begin_warm_read(key)
    }

    pub(crate) fn finish_warm_read(&self, reader_id: u64) {
        self.linearize().finish_warm_read(reader_id);
    }

    pub(crate) fn fail_warm_read(&self, reader_id: u64) {
        self.linearize().fail_warm_read(reader_id);
    }

    pub(crate) fn abandon_warm_read(&self, reader_id: u64) {
        self.linearize().abandon_warm_read(reader_id);
    }

    pub(crate) fn claim_server(&self, key: &HandleTopicKey, generation: u64) {
        self.inner.lock().push(Event::ClaimServer {
            key: topic_key(key),
            generation,
        });
    }

    pub(crate) fn begin_connection(&self, key: &HandleTopicKey, owner: HandleTopicOwner) {
        self.inner.lock().push(Event::BeginConnection {
            key: topic_key(key),
            owner: owner_wire(owner),
        });
    }

    pub(crate) fn reuse_committed_connection(&self, key: &HandleTopicKey, owner: HandleTopicOwner) {
        self.inner.lock().push(Event::ReuseCommittedConnection {
            key: topic_key(key),
            owner: owner_wire(owner),
        });
    }

    pub(crate) fn commit_connection(&self, key: &HandleTopicKey, owner: HandleTopicOwner) {
        self.inner.lock().push(Event::CommitConnection {
            key: topic_key(key),
            owner: owner_wire(owner),
        });
    }

    pub(crate) fn rollback_connection(&self, key: &HandleTopicKey, owner: HandleTopicOwner) {
        self.inner.lock().push(Event::RollbackConnection {
            key: topic_key(key),
            owner: owner_wire(owner),
        });
    }

    pub(crate) fn disconnect(&self, key: &HandleTopicKey, owner: HandleTopicOwner) {
        self.linearize().disconnect(key, owner);
    }

    pub(crate) fn detach_generation(&self, generation: u64) {
        self.linearize().detach_generation(generation);
    }

    pub(crate) fn drain_pending(&self, token: TokenWire, runtime_id: u64, reusable: bool) {
        let mut machine = self.inner.lock();
        if reusable {
            machine.push(Event::DrainPendingReuse {
                token,
                runtime_id,
                next_generation: token.generation.saturating_add(1),
            });
        } else {
            machine.push(Event::DrainPendingRetire { token, runtime_id });
        }
    }

    pub(crate) fn drain_published(&self, token: TokenWire, reusable: bool) {
        let mut machine = self.inner.lock();
        if reusable {
            machine.push(Event::DrainPublishedReuse {
                token,
                next_generation: token.generation.saturating_add(1),
            });
        } else {
            machine.push(Event::DrainPublishedRetire { token });
        }
    }

    pub(crate) fn seal_for_close(&self) {
        self.linearize().seal_for_close();
    }

    pub(crate) fn close_registry(&self) {
        self.inner.lock().push(Event::CloseRegistry);
    }

    pub(crate) fn finish_close(&self) {
        self.inner.lock().push(Event::FinishClose);
    }

    pub(crate) fn mark_returned_success(&self) {
        self.inner.lock().returned_success = true;
    }

    pub(crate) fn trace_json(&self) -> String {
        let machine = self.inner.lock();
        serde_json::to_string_pretty(&TraceDocument {
            schema_version: SCHEMA_VERSION,
            initial: InitialWire {
                session: machine.session,
            },
            events: machine.events.clone(),
            trace_truncated: machine.trace_truncated,
            outcome: if machine.returned_success {
                "returned_success"
            } else {
                "in_progress"
            },
        })
        .expect("H4 handle trace serialization")
    }
}

fn topic_key(key: &HandleTopicKey) -> FormulaRevisionKeyWire {
    match key {
        HandleTopicKey::Formula(FormulaRevisionKey {
            caller,
            udf_id,
            inputs,
        }) => FormulaRevisionKeyWire {
            sheet_id: caller.sheet_id as u64,
            row: caller.row,
            column: caller.column,
            udf_id: (*udf_id).to_owned(),
            input_fingerprint: encode_digest(inputs.as_bytes()),
        },
    }
}

fn encode_digest(digest: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn owner_wire(owner: HandleTopicOwner) -> OwnerWire {
    OwnerWire {
        server_generation: owner.server_generation,
        topic_id: owner.topic_id,
    }
}
