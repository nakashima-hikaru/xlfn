//! Passive shutdown observations emitted after concrete operational
//! transitions. Certificate observations retain lifecycle order; activity
//! observations are ownership evidence and do not define a total order.
//!
//! This module deliberately contains no shutdown state machine.  The runtime
//! owns concrete lifecycle state, ownership, and quiescence certificates; the
//! Lean shutdown model owns the abstract phase machine and event
//! preconditions.  Rust only records the wire events that the production path
//! actually emitted.

#![allow(
    dead_code,
    reason = "the passive wire schema covers events emitted by all supported feature profiles"
)]

use parking_lot::Mutex;
use serde::Serialize;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ACTIVITY_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize)]
#[serde(transparent)]
pub(crate) struct ActivityId(u64);

impl ActivityId {
    pub(crate) fn fresh() -> Self {
        let id = NEXT_ACTIVITY_ID.fetch_add(1, Ordering::Relaxed);
        if id == 0 {
            std::process::abort();
        }
        Self(id)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ShutdownFailure {
    BoundaryPanic,
    UnregisterFailed,
    ReturnShutdownFailed,
    AsyncShutdownFailed,
    RtdShutdownFailed,
    HandleShutdownFailed,
    GenerationEscaped,
    AddinShutdownFailed,
    DiagnosticsShutdownFailed,
    InvariantViolation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Completion {
    Completed,
    Canceled,
    Failed,
}

/// Resource evidence projected from the operational runtime for the Lean wire
/// format.  This is a data transfer object: it intentionally has no
/// predicates or transition logic.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ShutdownResources {
    pub(crate) ingress_open: bool,
    pub(crate) external_entries: u64,
    pub(crate) registrations: u64,
    pub(crate) event_registrations: u64,
    pub(crate) registration_state_known: bool,
    pub(crate) callback_gate_open: bool,
    pub(crate) active_calls: u64,
    pub(crate) return_blocks: u64,
    pub(crate) return_blocks_in_free: u64,
    pub(crate) return_free_operations: u64,
    pub(crate) async_tasks: u64,
    pub(crate) async_executor_running: bool,
    pub(crate) rtd_operations: u64,
    pub(crate) subscriptions: u64,
    pub(crate) callbacks: u64,
    pub(crate) rtd_class_factories: u64,
    pub(crate) rtd_servers: u64,
    pub(crate) rtd_server_locks: u64,
    pub(crate) handles: u64,
    pub(crate) handle_pins: u64,
    pub(crate) handle_objects: u64,
    pub(crate) generation_unique: bool,
    pub(crate) addin_quiesced: bool,
    pub(crate) generation_owned_by_runtime: bool,
    pub(crate) diagnostics_pending: u64,
    pub(crate) diagnostics_running: bool,
    pub(crate) cleanup_issues: u64,
}

impl ShutdownResources {
    pub(crate) const fn opened(registrations: u64, event_registrations: u64) -> Self {
        Self {
            ingress_open: true,
            external_entries: 0,
            registrations,
            event_registrations,
            registration_state_known: true,
            callback_gate_open: true,
            active_calls: 0,
            return_blocks: 0,
            return_blocks_in_free: 0,
            return_free_operations: 0,
            async_tasks: 0,
            async_executor_running: false,
            rtd_operations: 0,
            subscriptions: 0,
            callbacks: 0,
            rtd_class_factories: 0,
            rtd_servers: 0,
            rtd_server_locks: 0,
            handles: 0,
            handle_pins: 0,
            handle_objects: 0,
            generation_unique: false,
            addin_quiesced: false,
            generation_owned_by_runtime: true,
            diagnostics_pending: 0,
            diagnostics_running: false,
            cleanup_issues: 0,
        }
    }

    /// Projection from a concrete quiescence certificate to the wire DTO.
    pub(crate) const fn quiescent_snapshot() -> Self {
        Self {
            ingress_open: false,
            external_entries: 0,
            registrations: 0,
            event_registrations: 0,
            registration_state_known: true,
            callback_gate_open: false,
            active_calls: 0,
            return_blocks: 0,
            return_blocks_in_free: 0,
            return_free_operations: 0,
            async_tasks: 0,
            async_executor_running: false,
            rtd_operations: 0,
            subscriptions: 0,
            callbacks: 0,
            rtd_class_factories: 0,
            rtd_servers: 0,
            rtd_server_locks: 0,
            handles: 0,
            handle_pins: 0,
            handle_objects: 0,
            generation_unique: true,
            addin_quiesced: true,
            generation_owned_by_runtime: false,
            diagnostics_pending: 0,
            diagnostics_running: false,
            cleanup_issues: 0,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ShutdownEvent {
    RegisterFunction,
    UnregisterFunction,
    RegisterEvent,
    UnregisterEvent,
    EnterExternal { id: ActivityId },
    LeaveExternal { id: ActivityId },
    EnterCall { id: ActivityId },
    LeaveCall { id: ActivityId },
    CreateReturnBlock,
    BeginReturnFree,
    ReleaseReturnBlock,
    EndReturnFree,
    StartAsyncExecutor,
    StartAsyncTask,
    EndAsyncTask(Completion),
    StopAsyncExecutor,
    BeginRtdOperation,
    EndRtdOperation,
    AddSubscription,
    RemoveSubscription,
    BeginCallback,
    EndCallback,
    AddRtdClassFactory,
    RemoveRtdClassFactory,
    AddRtdServer,
    RemoveRtdServer,
    LockRtdServer,
    UnlockRtdServer,
    AddHandle,
    RemoveHandle,
    AddHandleObject,
    RemoveHandleObject,
    AddHandlePin,
    RemoveHandlePin,
    StartDiagnostics,
    EnqueueDiagnostic,
    FlushDiagnostic,
    DiscardDiagnostic,
    StopDiagnostics,
    RecordCleanupIssue,
    BeginClose,
    CallsDrained,
    ReturnsDrained,
    AsyncDrained,
    SubscriptionsDrained,
    CloseCallbackGate,
    HostDetached,
    ProveGenerationUnique,
    ProveAddinQuiesced,
    GenerationReclaimed,
    HandlesDrained,
    DiagnosticsDrained,
    RtdDrained,
    FinishClose,
    Quarantine(ShutdownFailure),
    FailStop(ShutdownFailure),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ActivityEvent {
    EnterExternal { id: ActivityId },
    LeaveExternal { id: ActivityId },
    EnterCall { id: ActivityId },
    LeaveCall { id: ActivityId },
    CreateReturnBlock,
    BeginReturnFree,
    ReleaseReturnBlock,
    EndReturnFree,
    StartAsyncTask,
    EndAsyncTask(Completion),
    BeginRtdOperation,
    EndRtdOperation,
    AddSubscription,
    RemoveSubscription,
    BeginCallback,
    EndCallback,
    AddRtdClassFactory,
    RemoveRtdClassFactory,
    AddRtdServer,
    RemoveRtdServer,
    LockRtdServer,
    UnlockRtdServer,
    AddHandle,
    RemoveHandle,
    AddHandleObject,
    RemoveHandleObject,
    AddHandlePin,
    RemoveHandlePin,
    EnqueueDiagnostic,
    FlushDiagnostic,
    DiscardDiagnostic,
    RecordCleanupIssue,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum CertificateEvent {
    RegisterFunction,
    UnregisterFunction,
    RegisterEvent,
    UnregisterEvent,
    StartAsyncExecutor,
    StopAsyncExecutor,
    StartDiagnostics,
    StopDiagnostics,
    BeginClose,
    CallsDrained,
    ReturnsDrained,
    AsyncDrained,
    SubscriptionsDrained,
    CloseCallbackGate,
    HostDetached,
    ProveGenerationUnique,
    ProveAddinQuiesced,
    GenerationReclaimed,
    HandlesDrained,
    DiagnosticsDrained,
    RtdDrained,
    FinishClose,
    Quarantine(ShutdownFailure),
    FailStop(ShutdownFailure),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ShutdownObservation {
    Activity(ActivityEvent),
    Certificate(CertificateEvent),
}

impl ShutdownEvent {
    fn into_observation(self) -> ShutdownObservation {
        match self {
            Self::EnterExternal { id } => {
                ShutdownObservation::Activity(ActivityEvent::EnterExternal { id })
            }
            Self::LeaveExternal { id } => {
                ShutdownObservation::Activity(ActivityEvent::LeaveExternal { id })
            }
            Self::EnterCall { id } => {
                ShutdownObservation::Activity(ActivityEvent::EnterCall { id })
            }
            Self::LeaveCall { id } => {
                ShutdownObservation::Activity(ActivityEvent::LeaveCall { id })
            }
            Self::RegisterFunction => {
                ShutdownObservation::Certificate(CertificateEvent::RegisterFunction)
            }
            Self::UnregisterFunction => {
                ShutdownObservation::Certificate(CertificateEvent::UnregisterFunction)
            }
            Self::RegisterEvent => {
                ShutdownObservation::Certificate(CertificateEvent::RegisterEvent)
            }
            Self::UnregisterEvent => {
                ShutdownObservation::Certificate(CertificateEvent::UnregisterEvent)
            }
            Self::StartAsyncExecutor => {
                ShutdownObservation::Certificate(CertificateEvent::StartAsyncExecutor)
            }
            Self::StopAsyncExecutor => {
                ShutdownObservation::Certificate(CertificateEvent::StopAsyncExecutor)
            }
            Self::StartDiagnostics => {
                ShutdownObservation::Certificate(CertificateEvent::StartDiagnostics)
            }
            Self::StopDiagnostics => {
                ShutdownObservation::Certificate(CertificateEvent::StopDiagnostics)
            }
            Self::BeginClose => ShutdownObservation::Certificate(CertificateEvent::BeginClose),
            Self::CallsDrained => ShutdownObservation::Certificate(CertificateEvent::CallsDrained),
            Self::ReturnsDrained => {
                ShutdownObservation::Certificate(CertificateEvent::ReturnsDrained)
            }
            Self::AsyncDrained => ShutdownObservation::Certificate(CertificateEvent::AsyncDrained),
            Self::SubscriptionsDrained => {
                ShutdownObservation::Certificate(CertificateEvent::SubscriptionsDrained)
            }
            Self::CloseCallbackGate => {
                ShutdownObservation::Certificate(CertificateEvent::CloseCallbackGate)
            }
            Self::HostDetached => ShutdownObservation::Certificate(CertificateEvent::HostDetached),
            Self::ProveGenerationUnique => {
                ShutdownObservation::Certificate(CertificateEvent::ProveGenerationUnique)
            }
            Self::ProveAddinQuiesced => {
                ShutdownObservation::Certificate(CertificateEvent::ProveAddinQuiesced)
            }
            Self::GenerationReclaimed => {
                ShutdownObservation::Certificate(CertificateEvent::GenerationReclaimed)
            }
            Self::HandlesDrained => {
                ShutdownObservation::Certificate(CertificateEvent::HandlesDrained)
            }
            Self::DiagnosticsDrained => {
                ShutdownObservation::Certificate(CertificateEvent::DiagnosticsDrained)
            }
            Self::RtdDrained => ShutdownObservation::Certificate(CertificateEvent::RtdDrained),
            Self::FinishClose => ShutdownObservation::Certificate(CertificateEvent::FinishClose),
            Self::Quarantine(reason) => {
                ShutdownObservation::Certificate(CertificateEvent::Quarantine(reason))
            }
            Self::FailStop(reason) => {
                ShutdownObservation::Certificate(CertificateEvent::FailStop(reason))
            }
            Self::CreateReturnBlock => {
                ShutdownObservation::Activity(ActivityEvent::CreateReturnBlock)
            }
            Self::BeginReturnFree => ShutdownObservation::Activity(ActivityEvent::BeginReturnFree),
            Self::ReleaseReturnBlock => {
                ShutdownObservation::Activity(ActivityEvent::ReleaseReturnBlock)
            }
            Self::EndReturnFree => ShutdownObservation::Activity(ActivityEvent::EndReturnFree),
            Self::StartAsyncTask => ShutdownObservation::Activity(ActivityEvent::StartAsyncTask),
            Self::EndAsyncTask(completion) => {
                ShutdownObservation::Activity(ActivityEvent::EndAsyncTask(completion))
            }
            Self::BeginRtdOperation => {
                ShutdownObservation::Activity(ActivityEvent::BeginRtdOperation)
            }
            Self::EndRtdOperation => ShutdownObservation::Activity(ActivityEvent::EndRtdOperation),
            Self::AddSubscription => ShutdownObservation::Activity(ActivityEvent::AddSubscription),
            Self::RemoveSubscription => {
                ShutdownObservation::Activity(ActivityEvent::RemoveSubscription)
            }
            Self::BeginCallback => ShutdownObservation::Activity(ActivityEvent::BeginCallback),
            Self::EndCallback => ShutdownObservation::Activity(ActivityEvent::EndCallback),
            Self::AddRtdClassFactory => {
                ShutdownObservation::Activity(ActivityEvent::AddRtdClassFactory)
            }
            Self::RemoveRtdClassFactory => {
                ShutdownObservation::Activity(ActivityEvent::RemoveRtdClassFactory)
            }
            Self::AddRtdServer => ShutdownObservation::Activity(ActivityEvent::AddRtdServer),
            Self::RemoveRtdServer => ShutdownObservation::Activity(ActivityEvent::RemoveRtdServer),
            Self::LockRtdServer => ShutdownObservation::Activity(ActivityEvent::LockRtdServer),
            Self::UnlockRtdServer => ShutdownObservation::Activity(ActivityEvent::UnlockRtdServer),
            Self::AddHandle => ShutdownObservation::Activity(ActivityEvent::AddHandle),
            Self::RemoveHandle => ShutdownObservation::Activity(ActivityEvent::RemoveHandle),
            Self::AddHandleObject => ShutdownObservation::Activity(ActivityEvent::AddHandleObject),
            Self::RemoveHandleObject => {
                ShutdownObservation::Activity(ActivityEvent::RemoveHandleObject)
            }
            Self::AddHandlePin => ShutdownObservation::Activity(ActivityEvent::AddHandlePin),
            Self::RemoveHandlePin => ShutdownObservation::Activity(ActivityEvent::RemoveHandlePin),
            Self::EnqueueDiagnostic => {
                ShutdownObservation::Activity(ActivityEvent::EnqueueDiagnostic)
            }
            Self::FlushDiagnostic => ShutdownObservation::Activity(ActivityEvent::FlushDiagnostic),
            Self::DiscardDiagnostic => {
                ShutdownObservation::Activity(ActivityEvent::DiscardDiagnostic)
            }
            Self::RecordCleanupIssue => {
                ShutdownObservation::Activity(ActivityEvent::RecordCleanupIssue)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TraceOutcome {
    InProgress,
    ReturnedSuccess,
    Quarantined,
    FailStopped,
}

impl TraceOutcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::InProgress => "in_progress",
            Self::ReturnedSuccess => "returned_success",
            Self::Quarantined => "quarantined",
            Self::FailStopped => "fail_stopped",
        }
    }
}

#[cfg(any(test, feature = "refinement"))]
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct ShutdownTrace {
    pub(crate) generation: u64,
    pub(crate) initial: ShutdownResources,
    pub(crate) activities: Vec<ActivityEvent>,
    pub(crate) certificates: Vec<CertificateEvent>,
    #[serde(rename = "trace_truncated")]
    pub(crate) trace_truncated: bool,
    pub(crate) outcome: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TraceError {
    InvalidGeneration,
    SessionAlreadyActive,
}

impl fmt::Display for TraceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidGeneration => formatter.write_str("shutdown trace generation is zero"),
            Self::SessionAlreadyActive => {
                formatter.write_str("shutdown trace session is already active")
            }
        }
    }
}

const MAX_TRACE_EVENTS: usize = 16_384;

struct TraceSession {
    generation: u64,
    initial: ShutdownResources,
    activities: Vec<ActivityEvent>,
    certificates: Vec<CertificateEvent>,
    trace_truncated: bool,
    outcome: TraceOutcome,
}

enum RecorderState {
    Idle,
    Recording(TraceSession),
    Terminal(TraceSession),
}

/// Passive event sink for the independent Lean shutdown specification.
pub(crate) struct ShutdownTraceRecorder {
    state: Mutex<RecorderState>,
    composition: Mutex<Option<Arc<crate::composition_refinement::CompositionTrace>>>,
}

impl ShutdownTraceRecorder {
    pub(crate) const fn new() -> Self {
        Self {
            state: Mutex::new(RecorderState::Idle),
            composition: Mutex::new(None),
        }
    }

    pub(crate) fn set_composition(
        &self,
        composition: Arc<crate::composition_refinement::CompositionTrace>,
    ) {
        *self.composition.lock() = Some(composition);
    }

    pub(crate) fn begin(
        &self,
        generation: u64,
        initial: ShutdownResources,
    ) -> Result<(), TraceError> {
        if generation == 0 {
            return Err(TraceError::InvalidGeneration);
        }
        let mut state = self.state.lock();
        if matches!(*state, RecorderState::Recording(_)) {
            return Err(TraceError::SessionAlreadyActive);
        }
        *state = RecorderState::Recording(TraceSession {
            generation,
            initial,
            activities: Vec::new(),
            certificates: Vec::new(),
            trace_truncated: false,
            outcome: TraceOutcome::InProgress,
        });
        Ok(())
    }

    /// Record an observation that the operational path has already produced.
    ///
    /// No abstract phase, resource predicate, or event precondition is
    /// evaluated here.  The event is also lifted into the composition trace
    /// as a wire observation, preserving the existing composition boundary.
    pub(crate) fn record(&self, event: ShutdownEvent) {
        let observation = event.into_observation();
        let mut lifted_certificate = None;
        let mut terminal_outcome = None;
        {
            let mut state = self.state.lock();
            let RecorderState::Recording(session) = &mut *state else {
                return;
            };
            match observation {
                ShutdownObservation::Activity(activity) => {
                    if session.activities.len() < MAX_TRACE_EVENTS {
                        session.activities.push(activity);
                    } else {
                        session.trace_truncated = true;
                    }
                }
                ShutdownObservation::Certificate(certificate) => {
                    if session.certificates.len() < MAX_TRACE_EVENTS {
                        session.certificates.push(certificate.clone());
                    } else {
                        session.trace_truncated = true;
                    }
                    terminal_outcome = match &certificate {
                        CertificateEvent::Quarantine(_) => Some(TraceOutcome::Quarantined),
                        CertificateEvent::FailStop(_) => Some(TraceOutcome::FailStopped),
                        _ => None,
                    };
                    if !matches!(&certificate, CertificateEvent::FinishClose) {
                        lifted_certificate = Some(certificate);
                    }
                }
            }
            if let Some(outcome) = terminal_outcome {
                session.outcome = outcome;
            }
        }
        if terminal_outcome.is_some() {
            let mut state = self.state.lock();
            let previous = std::mem::replace(&mut *state, RecorderState::Idle);
            if let RecorderState::Recording(session) = previous {
                *state = RecorderState::Terminal(session);
            }
        }
        if let Some(certificate) = lifted_certificate
            && let Some(composition) = self.composition.lock().as_ref().cloned()
        {
            composition
                .record(crate::composition_refinement::CompositionEvent::LiftShutdown(certificate));
        }
    }

    pub(crate) fn mark_returned_success(&self) {
        let mut state = self.state.lock();
        let previous = std::mem::replace(&mut *state, RecorderState::Idle);
        match previous {
            RecorderState::Recording(mut session) => {
                session.outcome = TraceOutcome::ReturnedSuccess;
                *state = RecorderState::Terminal(session);
            }
            other => *state = other,
        }
    }

    pub(crate) fn active(&self) -> bool {
        matches!(*self.state.lock(), RecorderState::Recording(_))
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn trace(&self) -> Option<ShutdownTrace> {
        let state = self.state.lock();
        let session = match &*state {
            RecorderState::Recording(session) | RecorderState::Terminal(session) => session,
            RecorderState::Idle => return None,
        };
        Some(ShutdownTrace {
            generation: session.generation,
            initial: session.initial.clone(),
            activities: session.activities.clone(),
            certificates: session.certificates.clone(),
            trace_truncated: session.trace_truncated,
            outcome: session.outcome.as_str().to_owned(),
        })
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn trace_json(&self) -> Option<String> {
        self.trace().map(|trace| {
            serde_json::to_string_pretty(&trace).expect("shutdown trace serialization")
        })
    }

    #[cfg(test)]
    pub(crate) fn activities(&self) -> Vec<ActivityEvent> {
        let state = self.state.lock();
        match &*state {
            RecorderState::Recording(session) | RecorderState::Terminal(session) => {
                session.activities.clone()
            }
            RecorderState::Idle => Vec::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn certificates(&self) -> Vec<CertificateEvent> {
        let state = self.state.lock();
        match &*state {
            RecorderState::Recording(session) | RecorderState::Terminal(session) => {
                session.certificates.clone()
            }
            RecorderState::Idle => Vec::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn disable_for_test(&self) {
        *self.state.lock() = RecorderState::Idle;
    }
}

pub(crate) type ShutdownTraceHandle = Arc<ShutdownTraceRecorder>;

#[cfg(test)]
mod tests {
    use super::*;

    fn recorder() -> ShutdownTraceRecorder {
        ShutdownTraceRecorder::new()
    }

    #[test]
    fn recorder_preserves_wire_events_without_replaying_semantics() {
        let recorder = recorder();
        recorder.begin(42, ShutdownResources::opened(0, 0)).unwrap();
        recorder.record(ShutdownEvent::BeginClose);
        recorder.record(ShutdownEvent::FinishClose);
        assert_eq!(
            recorder.certificates(),
            [CertificateEvent::BeginClose, CertificateEvent::FinishClose]
        );
        assert!(recorder.active());
    }

    #[test]
    fn terminal_observation_stops_recording() {
        let recorder = recorder();
        recorder.begin(42, ShutdownResources::opened(0, 0)).unwrap();
        recorder.record(ShutdownEvent::Quarantine(ShutdownFailure::BoundaryPanic));
        recorder.record(ShutdownEvent::LeaveExternal {
            id: ActivityId::fresh(),
        });
        assert!(!recorder.active());
        assert!(recorder.certificates().len() == 1);
        assert_eq!(recorder.trace().unwrap().outcome, "quarantined");
    }

    #[test]
    fn activity_observations_do_not_require_append_order() {
        let recorder = recorder();
        let id = ActivityId::fresh();
        recorder.begin(42, ShutdownResources::opened(0, 0)).unwrap();
        recorder.record(ShutdownEvent::LeaveCall { id });
        recorder.record(ShutdownEvent::EnterCall { id });

        assert_eq!(
            recorder.activities(),
            [
                ActivityEvent::LeaveCall { id },
                ActivityEvent::EnterCall { id }
            ]
        );
    }

    #[test]
    fn success_is_an_observed_recorder_outcome() {
        let recorder = recorder();
        recorder.begin(42, ShutdownResources::opened(0, 0)).unwrap();
        recorder.record(ShutdownEvent::FinishClose);
        recorder.mark_returned_success();
        assert!(!recorder.active());
        assert_eq!(recorder.trace().unwrap().outcome, "returned_success");
    }

    #[test]
    fn recorder_rejects_zero_generation_and_overlapping_sessions() {
        let recorder = recorder();
        assert_eq!(
            recorder.begin(0, ShutdownResources::opened(0, 0)),
            Err(TraceError::InvalidGeneration)
        );
        recorder.begin(1, ShutdownResources::opened(0, 0)).unwrap();
        assert_eq!(
            recorder.begin(2, ShutdownResources::opened(0, 0)),
            Err(TraceError::SessionAlreadyActive)
        );
    }
}
