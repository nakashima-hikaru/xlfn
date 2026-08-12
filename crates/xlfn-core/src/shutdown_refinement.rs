#![allow(
    dead_code,
    reason = "Shutdown ghost model mirrors the complete Lean transition and trace schema; some states and serializers are exercised only by checker tests"
)]

use parking_lot::Mutex;
use serde::Serialize;
use std::fmt;
use std::sync::Arc;

#[cfg(any(test, feature = "shutdown-trace"))]
pub(crate) const SCHEMA_VERSION: u32 = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum GhostStage {
    DrainCalls,
    DrainReturns,
    DrainAsync,
    StopSubscriptions,
    DetachHost,
    CloseState,
    DrainHandles,
    StopDiagnostics,
    DrainRtd,
    Finalize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum GhostFailure {
    BoundaryPanic,
    UnregisterFailed,
    ReturnShutdownFailed,
    AsyncShutdownFailed,
    RtdShutdownFailed,
    HandleShutdownFailed,
    StateEscaped,
    AddinShutdownFailed,
    DiagnosticsShutdownFailed,
    InvariantViolation,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) enum GhostPhase {
    Open,
    Closing(GhostStage),
    Closed,
    FailStopped(GhostFailure),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Completion {
    Completed,
    Canceled,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GhostResources {
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
    pub(crate) handle_operations: u64,
    pub(crate) handles: u64,
    pub(crate) state_unique: bool,
    pub(crate) addin_quiesced: bool,
    pub(crate) state_owned_by_runtime: bool,
    pub(crate) diagnostics_pending: u64,
    pub(crate) diagnostics_running: bool,
    pub(crate) cleanup_issues: u64,
}

impl GhostResources {
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
            handle_operations: 0,
            handles: 0,
            state_unique: false,
            addin_quiesced: false,
            state_owned_by_runtime: true,
            diagnostics_pending: 0,
            diagnostics_running: false,
            cleanup_issues: 0,
        }
    }

    fn host_detached(&self) -> bool {
        !self.ingress_open
            && self.registrations == 0
            && self.event_registrations == 0
            && self.registration_state_known
            && !self.callback_gate_open
    }

    fn calls_drained(&self) -> bool {
        self.external_entries == 0 && self.active_calls == 0
    }

    fn returns_drained(&self) -> bool {
        self.return_blocks == 0
            && self.return_blocks_in_free == 0
            && self.return_free_operations == 0
    }

    fn async_drained(&self) -> bool {
        self.async_tasks == 0 && !self.async_executor_running
    }

    fn subscriptions_drained(&self) -> bool {
        self.subscriptions == 0 && self.callbacks == 0
    }

    fn rtd_drained(&self) -> bool {
        self.rtd_operations == 0
            && self.rtd_class_factories == 0
            && self.rtd_servers == 0
            && self.rtd_server_locks == 0
    }

    fn handles_drained(&self) -> bool {
        self.handle_operations == 0 && self.handles == 0
    }

    fn state_closed(&self) -> bool {
        self.state_unique && self.addin_quiesced && !self.state_owned_by_runtime
    }

    fn diagnostics_drained(&self) -> bool {
        self.diagnostics_pending == 0 && !self.diagnostics_running
    }

    fn quiescent(&self) -> bool {
        self.host_detached()
            && self.calls_drained()
            && self.returns_drained()
            && self.async_drained()
            && self.subscriptions_drained()
            && self.rtd_drained()
            && self.handles_drained()
            && self.state_closed()
            && self.diagnostics_drained()
    }

    fn producer_alive(&self) -> bool {
        self.external_entries != 0
            || self.active_calls != 0
            || self.return_free_operations != 0
            || self.async_tasks != 0
            || self.rtd_operations != 0
            || self.subscriptions != 0
            || self.callbacks != 0
            || self.handle_operations != 0
            || self.diagnostics_pending != 0
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct GhostState {
    pub(crate) generation: u64,
    pub(crate) phase: GhostPhase,
    pub(crate) resources: GhostResources,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum GhostEvent {
    RegisterFunction,
    UnregisterFunction,
    RegisterEvent,
    UnregisterEvent,
    EnterExternal,
    LeaveExternal,
    EnterCall,
    LeaveCall,
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
    BeginHandleOperation,
    EndHandleOperation,
    AddHandle,
    RemoveHandle,
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
    ProveStateUnique,
    ProveAddinQuiesced,
    StateClosed,
    HandlesDrained,
    DiagnosticsDrained,
    RtdDrained,
    FinishClose,
    FailStop(GhostFailure),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GhostOutcome {
    InProgress,
    ReturnedSuccess,
    FailStopped,
}

impl GhostOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::InProgress => "in_progress",
            Self::ReturnedSuccess => "returned_success",
            Self::FailStopped => "fail_stopped",
        }
    }
}

#[cfg(any(test, feature = "shutdown-trace"))]
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct GhostTrace {
    pub(crate) schema_version: u32,
    pub(crate) generation: u64,
    pub(crate) initial: GhostState,
    pub(crate) events: Vec<GhostEvent>,
    pub(crate) trace_truncated: bool,
    pub(crate) outcome: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum GhostViolation {
    NoGeneration,
    GenerationAlreadyStarted,
    WrongPhase {
        event: &'static str,
        phase: GhostPhase,
    },
    Precondition(&'static str),
    CounterUnderflow(&'static str),
    Terminal,
}

impl fmt::Display for GhostViolation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoGeneration => formatter.write_str("shutdown ghost generation is not active"),
            Self::GenerationAlreadyStarted => {
                formatter.write_str("shutdown ghost generation already started")
            }
            Self::WrongPhase { event, phase } => {
                write!(formatter, "event {event} is not valid in phase {phase:?}")
            }
            Self::Precondition(message) => formatter.write_str(message),
            Self::CounterUnderflow(counter) => write!(formatter, "counter underflow: {counter}"),
            Self::Terminal => formatter.write_str("shutdown ghost is terminal"),
        }
    }
}

fn increment(counter: &mut u64, name: &'static str) -> Result<(), GhostViolation> {
    *counter = counter
        .checked_add(1)
        .ok_or(GhostViolation::Precondition(name))?;
    Ok(())
}

fn decrement(counter: &mut u64, name: &'static str) -> Result<(), GhostViolation> {
    *counter = counter
        .checked_sub(1)
        .ok_or(GhostViolation::CounterUnderflow(name))?;
    Ok(())
}

fn live(phase: &GhostPhase) -> bool {
    matches!(phase, GhostPhase::Open | GhostPhase::Closing(_))
}

fn phase_is(phase: &GhostPhase, expected: GhostStage) -> bool {
    matches!(phase, GhostPhase::Closing(stage) if *stage == expected)
}

fn return_creation_allowed(phase: &GhostPhase) -> bool {
    matches!(
        phase,
        GhostPhase::Open | GhostPhase::Closing(GhostStage::DrainCalls)
    )
}

fn return_free_allowed(phase: &GhostPhase) -> bool {
    matches!(
        phase,
        GhostPhase::Open
            | GhostPhase::Closing(GhostStage::DrainCalls)
            | GhostPhase::Closing(GhostStage::DrainReturns)
    )
}

fn async_creation_allowed(phase: &GhostPhase) -> bool {
    matches!(
        phase,
        GhostPhase::Open
            | GhostPhase::Closing(GhostStage::DrainCalls)
            | GhostPhase::Closing(GhostStage::DrainReturns)
            | GhostPhase::Closing(GhostStage::DrainAsync)
    )
}

fn subscription_creation_allowed(phase: &GhostPhase) -> bool {
    matches!(
        phase,
        GhostPhase::Open
            | GhostPhase::Closing(GhostStage::DrainCalls)
            | GhostPhase::Closing(GhostStage::DrainReturns)
            | GhostPhase::Closing(GhostStage::DrainAsync)
            | GhostPhase::Closing(GhostStage::StopSubscriptions)
    )
}

fn rtd_creation_allowed(phase: &GhostPhase) -> bool {
    matches!(
        phase,
        GhostPhase::Open
            | GhostPhase::Closing(GhostStage::DrainCalls)
            | GhostPhase::Closing(GhostStage::DrainReturns)
            | GhostPhase::Closing(GhostStage::DrainAsync)
            | GhostPhase::Closing(GhostStage::StopSubscriptions)
            | GhostPhase::Closing(GhostStage::DetachHost)
            | GhostPhase::Closing(GhostStage::CloseState)
            | GhostPhase::Closing(GhostStage::DrainHandles)
            | GhostPhase::Closing(GhostStage::StopDiagnostics)
            | GhostPhase::Closing(GhostStage::DrainRtd)
    )
}

fn handle_creation_allowed(phase: &GhostPhase) -> bool {
    matches!(
        phase,
        GhostPhase::Open
            | GhostPhase::Closing(GhostStage::DrainCalls)
            | GhostPhase::Closing(GhostStage::DrainReturns)
            | GhostPhase::Closing(GhostStage::DrainAsync)
            | GhostPhase::Closing(GhostStage::StopSubscriptions)
            | GhostPhase::Closing(GhostStage::DetachHost)
            | GhostPhase::Closing(GhostStage::CloseState)
            | GhostPhase::Closing(GhostStage::DrainHandles)
    )
}

fn diagnostic_creation_allowed(phase: &GhostPhase) -> bool {
    matches!(
        phase,
        GhostPhase::Open
            | GhostPhase::Closing(GhostStage::DrainCalls)
            | GhostPhase::Closing(GhostStage::DrainReturns)
            | GhostPhase::Closing(GhostStage::DrainAsync)
            | GhostPhase::Closing(GhostStage::StopSubscriptions)
            | GhostPhase::Closing(GhostStage::DetachHost)
            | GhostPhase::Closing(GhostStage::CloseState)
            | GhostPhase::Closing(GhostStage::DrainHandles)
            | GhostPhase::Closing(GhostStage::StopDiagnostics)
    )
}

fn transition(source: &GhostState, event: &GhostEvent) -> Result<GhostState, GhostViolation> {
    let mut target = source.clone();
    let resources = &mut target.resources;

    match event {
        GhostEvent::RegisterFunction => {
            if source.phase != GhostPhase::Open || !resources.ingress_open {
                return Err(GhostViolation::Precondition(
                    "function registration requires open ingress",
                ));
            }
            increment(&mut resources.registrations, "registrations")?;
        }
        GhostEvent::UnregisterFunction => decrement(&mut resources.registrations, "registrations")?,
        GhostEvent::RegisterEvent => {
            if source.phase != GhostPhase::Open || !resources.ingress_open {
                return Err(GhostViolation::Precondition(
                    "event registration requires open ingress",
                ));
            }
            increment(&mut resources.event_registrations, "event registrations")?;
        }
        GhostEvent::UnregisterEvent => {
            decrement(&mut resources.event_registrations, "event registrations")?
        }
        GhostEvent::EnterExternal => {
            if source.phase != GhostPhase::Open || !resources.ingress_open {
                return Err(GhostViolation::Precondition(
                    "external entry requires open ingress",
                ));
            }
            increment(&mut resources.external_entries, "external entries")?;
        }
        GhostEvent::LeaveExternal => {
            decrement(&mut resources.external_entries, "external entries")?
        }
        GhostEvent::EnterCall => {
            if source.phase != GhostPhase::Open {
                return Err(GhostViolation::Precondition(
                    "call entry requires open phase",
                ));
            }
            increment(&mut resources.active_calls, "active calls")?;
        }
        GhostEvent::LeaveCall => decrement(&mut resources.active_calls, "active calls")?,
        GhostEvent::CreateReturnBlock => {
            if !return_creation_allowed(&source.phase) || resources.active_calls == 0 {
                return Err(GhostViolation::Precondition(
                    "return block requires an admitted call",
                ));
            }
            increment(&mut resources.return_blocks, "return blocks")?;
        }
        GhostEvent::BeginReturnFree => {
            if !return_free_allowed(&source.phase)
                || resources.return_blocks_in_free >= resources.return_blocks
            {
                return Err(GhostViolation::Precondition(
                    "return free requires an outstanding return block",
                ));
            }
            increment(
                &mut resources.return_blocks_in_free,
                "return blocks in free",
            )?;
            increment(
                &mut resources.return_free_operations,
                "return free operations",
            )?;
        }
        GhostEvent::ReleaseReturnBlock => {
            if !live(&source.phase) {
                return Err(GhostViolation::Terminal);
            }
            if resources.return_blocks == 0 || resources.return_blocks_in_free == 0 {
                return Err(GhostViolation::Precondition(
                    "return block release requires a block in free",
                ));
            }
            decrement(&mut resources.return_blocks, "return blocks")?;
            decrement(
                &mut resources.return_blocks_in_free,
                "return blocks in free",
            )?;
        }
        GhostEvent::EndReturnFree => {
            if !live(&source.phase) {
                return Err(GhostViolation::Terminal);
            }
            if resources.return_free_operations == 0
                || resources.return_blocks_in_free >= resources.return_free_operations
            {
                return Err(GhostViolation::Precondition(
                    "return free ends only after its block is released",
                ));
            }
            decrement(
                &mut resources.return_free_operations,
                "return free operations",
            )?;
        }
        GhostEvent::StartAsyncExecutor => {
            if source.phase != GhostPhase::Open || resources.async_executor_running {
                return Err(GhostViolation::Precondition(
                    "async executor must start from open and stopped",
                ));
            }
            resources.async_executor_running = true;
        }
        GhostEvent::StartAsyncTask => {
            if !async_creation_allowed(&source.phase)
                || !resources.async_executor_running
                || !resources.producer_alive()
            {
                return Err(GhostViolation::Precondition(
                    "async task requires a live executor and producer",
                ));
            }
            increment(&mut resources.async_tasks, "async tasks")?;
        }
        GhostEvent::EndAsyncTask(_) => decrement(&mut resources.async_tasks, "async tasks")?,
        GhostEvent::StopAsyncExecutor => {
            if !live(&source.phase)
                || resources.async_tasks != 0
                || !resources.async_executor_running
            {
                return Err(GhostViolation::Precondition(
                    "async executor stops only after its task registry drains",
                ));
            }
            resources.async_executor_running = false;
        }
        GhostEvent::BeginRtdOperation => {
            if source.phase != GhostPhase::Open || !resources.ingress_open {
                return Err(GhostViolation::Precondition(
                    "RTD operation admission requires open ingress",
                ));
            }
            increment(&mut resources.rtd_operations, "RTD operations")?;
        }
        GhostEvent::EndRtdOperation => decrement(&mut resources.rtd_operations, "RTD operations")?,
        GhostEvent::AddSubscription => {
            if !subscription_creation_allowed(&source.phase) || resources.rtd_operations == 0 {
                return Err(GhostViolation::Precondition(
                    "subscription requires an RTD operation",
                ));
            }
            increment(&mut resources.subscriptions, "subscriptions")?;
        }
        GhostEvent::RemoveSubscription => decrement(&mut resources.subscriptions, "subscriptions")?,
        GhostEvent::BeginCallback => {
            if !subscription_creation_allowed(&source.phase) || resources.subscriptions == 0 {
                return Err(GhostViolation::Precondition(
                    "callback requires a live subscription",
                ));
            }
            increment(&mut resources.callbacks, "callbacks")?;
        }
        GhostEvent::EndCallback => decrement(&mut resources.callbacks, "callbacks")?,
        GhostEvent::AddRtdClassFactory => {
            if !rtd_creation_allowed(&source.phase) || resources.rtd_operations == 0 {
                return Err(GhostViolation::Precondition(
                    "RTD class factory requires an RTD operation",
                ));
            }
            increment(&mut resources.rtd_class_factories, "RTD class factories")?;
        }
        GhostEvent::RemoveRtdClassFactory => {
            decrement(&mut resources.rtd_class_factories, "RTD class factories")?
        }
        GhostEvent::AddRtdServer => {
            if !rtd_creation_allowed(&source.phase) || resources.rtd_operations == 0 {
                return Err(GhostViolation::Precondition(
                    "RTD server requires an RTD operation",
                ));
            }
            increment(&mut resources.rtd_servers, "RTD servers")?;
        }
        GhostEvent::RemoveRtdServer => decrement(&mut resources.rtd_servers, "RTD servers")?,
        GhostEvent::LockRtdServer => {
            if !rtd_creation_allowed(&source.phase) || resources.rtd_class_factories == 0 {
                return Err(GhostViolation::Precondition(
                    "RTD server lock requires a class factory",
                ));
            }
            increment(&mut resources.rtd_server_locks, "RTD server locks")?;
        }
        GhostEvent::UnlockRtdServer => {
            decrement(&mut resources.rtd_server_locks, "RTD server locks")?
        }
        GhostEvent::BeginHandleOperation => {
            if !handle_creation_allowed(&source.phase) {
                return Err(GhostViolation::Precondition(
                    "handle operation is not allowed in this phase",
                ));
            }
            increment(&mut resources.handle_operations, "handle operations")?;
        }
        GhostEvent::EndHandleOperation => {
            decrement(&mut resources.handle_operations, "handle operations")?
        }
        GhostEvent::AddHandle => {
            if !handle_creation_allowed(&source.phase) || resources.handle_operations == 0 {
                return Err(GhostViolation::Precondition(
                    "handle value requires a handle operation",
                ));
            }
            increment(&mut resources.handles, "handles")?;
        }
        GhostEvent::RemoveHandle => decrement(&mut resources.handles, "handles")?,
        GhostEvent::StartDiagnostics => {
            if source.phase != GhostPhase::Open || resources.diagnostics_running {
                return Err(GhostViolation::Precondition(
                    "diagnostics must start from open and stopped",
                ));
            }
            resources.diagnostics_running = true;
        }
        GhostEvent::EnqueueDiagnostic => {
            if !diagnostic_creation_allowed(&source.phase) || !resources.diagnostics_running {
                return Err(GhostViolation::Precondition(
                    "diagnostic enqueue requires a running dispatcher",
                ));
            }
            increment(&mut resources.diagnostics_pending, "diagnostic queue")?;
        }
        GhostEvent::FlushDiagnostic => {
            decrement(&mut resources.diagnostics_pending, "diagnostic queue")?
        }
        GhostEvent::DiscardDiagnostic => {
            if !live(&source.phase) {
                return Err(GhostViolation::Terminal);
            }
            decrement(&mut resources.diagnostics_pending, "diagnostic queue")?
        }
        GhostEvent::StopDiagnostics => {
            if !live(&source.phase)
                || resources.diagnostics_pending != 0
                || !resources.diagnostics_running
            {
                return Err(GhostViolation::Precondition(
                    "diagnostics stop requires an empty running dispatcher",
                ));
            }
            resources.diagnostics_running = false;
        }
        GhostEvent::RecordCleanupIssue => {
            if !live(&source.phase) {
                return Err(GhostViolation::Terminal);
            }
            increment(&mut resources.cleanup_issues, "cleanup issues")?;
        }
        GhostEvent::BeginClose => {
            if source.phase != GhostPhase::Open || !resources.ingress_open {
                return Err(GhostViolation::Precondition(
                    "close begins only while ingress is open",
                ));
            }
            resources.ingress_open = false;
            target.phase = GhostPhase::Closing(GhostStage::DrainCalls);
        }
        GhostEvent::CallsDrained => {
            if !phase_is(&source.phase, GhostStage::DrainCalls) || !resources.calls_drained() {
                return Err(GhostViolation::Precondition(
                    "calls milestone requires drained ingress and call guards",
                ));
            }
            target.phase = GhostPhase::Closing(GhostStage::DrainReturns);
        }
        GhostEvent::ReturnsDrained => {
            if !phase_is(&source.phase, GhostStage::DrainReturns) || !resources.returns_drained() {
                return Err(GhostViolation::Precondition(
                    "returns milestone requires no blocks or free callbacks",
                ));
            }
            target.phase = GhostPhase::Closing(GhostStage::DrainAsync);
        }
        GhostEvent::AsyncDrained => {
            if !phase_is(&source.phase, GhostStage::DrainAsync) || !resources.async_drained() {
                return Err(GhostViolation::Precondition(
                    "async milestone requires joined executor",
                ));
            }
            target.phase = GhostPhase::Closing(GhostStage::StopSubscriptions);
        }
        GhostEvent::SubscriptionsDrained => {
            if !phase_is(&source.phase, GhostStage::StopSubscriptions)
                || !resources.subscriptions_drained()
            {
                return Err(GhostViolation::Precondition(
                    "subscription milestone requires no subscriptions or callbacks",
                ));
            }
            target.phase = GhostPhase::Closing(GhostStage::DetachHost);
        }
        GhostEvent::CloseCallbackGate => {
            if !phase_is(&source.phase, GhostStage::DetachHost) || !resources.callback_gate_open {
                return Err(GhostViolation::Precondition(
                    "callback gate must close exactly once during host detachment",
                ));
            }
            resources.callback_gate_open = false;
        }
        GhostEvent::HostDetached => {
            if !phase_is(&source.phase, GhostStage::DetachHost) || !resources.host_detached() {
                return Err(GhostViolation::Precondition(
                    "host milestone requires known empty registrations and closed callback gate",
                ));
            }
            target.phase = GhostPhase::Closing(GhostStage::CloseState);
        }
        GhostEvent::ProveStateUnique => {
            if !phase_is(&source.phase, GhostStage::CloseState) || resources.state_unique {
                return Err(GhostViolation::Precondition(
                    "state uniqueness must be proven exactly once during state close",
                ));
            }
            resources.state_unique = true;
        }
        GhostEvent::ProveAddinQuiesced => {
            if !phase_is(&source.phase, GhostStage::CloseState) || resources.addin_quiesced {
                return Err(GhostViolation::Precondition(
                    "Add-in quiescence must be proven exactly once during state close",
                ));
            }
            resources.addin_quiesced = true;
        }
        GhostEvent::StateClosed => {
            if !phase_is(&source.phase, GhostStage::CloseState)
                || !resources.state_unique
                || !resources.addin_quiesced
                || !resources.state_owned_by_runtime
            {
                return Err(GhostViolation::Precondition(
                    "state milestone requires unique quiesced runtime-owned state",
                ));
            }
            resources.state_owned_by_runtime = false;
            target.phase = GhostPhase::Closing(GhostStage::DrainHandles);
        }
        GhostEvent::HandlesDrained => {
            if !phase_is(&source.phase, GhostStage::DrainHandles) || !resources.handles_drained() {
                return Err(GhostViolation::Precondition(
                    "handle milestone requires no operations or values",
                ));
            }
            target.phase = GhostPhase::Closing(GhostStage::StopDiagnostics);
        }
        GhostEvent::DiagnosticsDrained => {
            if !phase_is(&source.phase, GhostStage::StopDiagnostics)
                || !resources.diagnostics_drained()
            {
                return Err(GhostViolation::Precondition(
                    "diagnostic milestone requires joined empty dispatcher",
                ));
            }
            target.phase = GhostPhase::Closing(GhostStage::DrainRtd);
        }
        GhostEvent::RtdDrained => {
            if !phase_is(&source.phase, GhostStage::DrainRtd) || !resources.rtd_drained() {
                return Err(GhostViolation::Precondition(
                    "RTD milestone requires module quiescence",
                ));
            }
            target.phase = GhostPhase::Closing(GhostStage::Finalize);
        }
        GhostEvent::FinishClose => {
            if !phase_is(&source.phase, GhostStage::Finalize) || !resources.quiescent() {
                return Err(GhostViolation::Precondition(
                    "successful close requires complete quiescence",
                ));
            }
            target.phase = GhostPhase::Closed;
        }
        GhostEvent::FailStop(reason) => {
            if !live(&source.phase) {
                return Err(GhostViolation::Terminal);
            }
            target.phase = GhostPhase::FailStopped(*reason);
        }
    }

    Ok(target)
}

pub(crate) struct GhostMachine {
    initial: GhostState,
    state: GhostState,
    #[cfg(any(test, feature = "shutdown-trace"))]
    events: Vec<GhostEvent>,
    #[cfg(any(test, feature = "shutdown-trace"))]
    trace_truncated: bool,
    returned_success: bool,
    active: bool,
}

#[cfg(any(test, feature = "shutdown-trace"))]
const MAX_TRACE_EVENTS: usize = 16_384;

impl GhostMachine {
    const fn empty_state() -> GhostState {
        GhostState {
            generation: 0,
            phase: GhostPhase::Closed,
            resources: GhostResources::opened(0, 0),
        }
    }

    pub(crate) const fn new() -> Self {
        Self {
            initial: Self::empty_state(),
            state: Self::empty_state(),
            #[cfg(any(test, feature = "shutdown-trace"))]
            events: Vec::new(),
            #[cfg(any(test, feature = "shutdown-trace"))]
            trace_truncated: false,
            returned_success: false,
            active: false,
        }
    }

    pub(crate) fn begin_generation(
        &mut self,
        generation: u64,
        resources: GhostResources,
    ) -> Result<(), GhostViolation> {
        if self.active {
            return Err(GhostViolation::GenerationAlreadyStarted);
        }
        let state = GhostState {
            generation,
            phase: GhostPhase::Open,
            resources,
        };
        self.initial = state.clone();
        self.state = state;
        #[cfg(any(test, feature = "shutdown-trace"))]
        {
            // Releasing the old vector prevents a high-water mark from being
            // retained across reopen cycles.
            self.events = Vec::new();
            self.trace_truncated = false;
        }
        self.returned_success = false;
        self.active = true;
        Ok(())
    }

    pub(crate) fn apply(&mut self, event: GhostEvent) -> Result<(), GhostViolation> {
        if !self.active {
            return Err(GhostViolation::NoGeneration);
        }
        if matches!(
            self.state.phase,
            GhostPhase::Closed | GhostPhase::FailStopped(_)
        ) {
            return Err(GhostViolation::Terminal);
        }
        let after = transition(&self.state, &event)?;
        self.state = after;
        #[cfg(any(test, feature = "shutdown-trace"))]
        if self.events.len() < MAX_TRACE_EVENTS {
            self.events.push(event);
        } else {
            self.trace_truncated = true;
        }
        Ok(())
    }

    pub(crate) fn record_returned_success(&mut self) -> Result<(), GhostViolation> {
        if !self.active {
            return Err(GhostViolation::NoGeneration);
        }
        if self.state.phase != GhostPhase::Closed {
            return Err(GhostViolation::Precondition(
                "returned success requires the closed ghost phase",
            ));
        }
        self.returned_success = true;
        self.active = false;
        Ok(())
    }

    pub(crate) fn fail_stop(&mut self, reason: GhostFailure) -> Result<(), GhostViolation> {
        if !self.active {
            return Ok(());
        }
        let result = self.apply(GhostEvent::FailStop(reason));
        if result.is_ok() {
            // Fail-stop is the terminal refinement event. Concrete cleanup
            // and error reporting may still run while the process is being
            // terminated, but those actions are outside this generation and
            // must not append events after the terminal event.
            self.active = false;
        }
        result
    }

    #[cfg(any(test, feature = "shutdown-trace"))]
    pub(crate) fn trace(&self) -> GhostTrace {
        let outcome = if self.returned_success {
            GhostOutcome::ReturnedSuccess
        } else if matches!(self.state.phase, GhostPhase::FailStopped(_)) {
            GhostOutcome::FailStopped
        } else {
            GhostOutcome::InProgress
        };
        GhostTrace {
            schema_version: SCHEMA_VERSION,
            generation: self.initial.generation,
            initial: self.initial.clone(),
            events: self.events.clone(),
            trace_truncated: self.trace_truncated,
            outcome: outcome.as_str().to_owned(),
        }
    }

    pub(crate) fn state(&self) -> &GhostState {
        &self.state
    }
}

pub(crate) struct ShutdownGhost {
    inner: Mutex<GhostMachine>,
    composition: Mutex<Option<Arc<crate::composition_refinement::CompositionTrace>>>,
}

impl ShutdownGhost {
    pub(crate) const fn new() -> Self {
        Self {
            inner: Mutex::new(GhostMachine::new()),
            composition: Mutex::new(None),
        }
    }

    pub(crate) fn set_composition(
        &self,
        composition: Arc<crate::composition_refinement::CompositionTrace>,
    ) {
        *self.composition.lock() = Some(composition);
    }

    pub(crate) fn begin_generation(
        &self,
        generation: u64,
        resources: GhostResources,
    ) -> Result<(), GhostViolation> {
        self.inner.lock().begin_generation(generation, resources)
    }

    pub(crate) fn apply(&self, event: GhostEvent) -> Result<(), GhostViolation> {
        let result = self.inner.lock().apply(event.clone());
        if result.is_ok()
            && !matches!(event, GhostEvent::FinishClose)
            && let Some(composition) = self.composition.lock().as_ref().cloned()
        {
            composition
                .record(crate::composition_refinement::CompositionEvent::LiftShutdown(event));
        }
        result
    }

    pub(crate) fn record_event(&self, event: GhostEvent) {
        if !self.active() {
            return;
        }
        self.apply(event)
            .unwrap_or_else(|violation| panic!("shutdown refinement violation: {violation}"));
    }

    pub(crate) fn fail_stop(&self, reason: GhostFailure) -> Result<(), GhostViolation> {
        if !self.active() {
            return Ok(());
        }
        self.apply(GhostEvent::FailStop(reason))
    }

    pub(crate) fn record_returned_success(&self) -> Result<(), GhostViolation> {
        self.inner.lock().record_returned_success()
    }

    #[cfg(any(test, feature = "shutdown-trace"))]
    pub(crate) fn trace_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(&self.inner.lock().trace())
    }

    pub(crate) fn state(&self) -> GhostState {
        self.inner.lock().state().clone()
    }

    pub(crate) fn active(&self) -> bool {
        self.inner.lock().active
    }

    #[cfg(test)]
    pub(crate) fn disable_for_test(&self) {
        self.inner.lock().active = false;
    }
}

pub(crate) type GhostHandle = Arc<ShutdownGhost>;

#[cfg(test)]
mod tests {
    use super::*;

    fn open_machine() -> GhostMachine {
        let mut machine = GhostMachine::new();
        machine
            .begin_generation(42, GhostResources::opened(0, 0))
            .unwrap();
        machine
    }

    #[test]
    fn transition_rejects_finish_before_quiescence() {
        let mut machine = open_machine();
        machine.apply(GhostEvent::BeginClose).unwrap();
        assert!(matches!(
            machine.apply(GhostEvent::FinishClose),
            Err(GhostViolation::Precondition(_))
        ));
    }

    #[test]
    fn overlapping_return_frees_claim_distinct_live_blocks() {
        let mut machine = open_machine();

        for event in [
            GhostEvent::EnterCall,
            GhostEvent::CreateReturnBlock,
            GhostEvent::LeaveCall,
            GhostEvent::BeginReturnFree,
            GhostEvent::ReleaseReturnBlock,
            GhostEvent::EnterCall,
            GhostEvent::CreateReturnBlock,
            GhostEvent::LeaveCall,
            GhostEvent::BeginReturnFree,
        ] {
            machine.apply(event).unwrap();
        }

        assert_eq!(machine.state().resources.return_blocks, 1);
        assert_eq!(machine.state().resources.return_blocks_in_free, 1);
        assert_eq!(machine.state().resources.return_free_operations, 2);

        for event in [
            GhostEvent::ReleaseReturnBlock,
            GhostEvent::EndReturnFree,
            GhostEvent::EndReturnFree,
        ] {
            machine.apply(event).unwrap();
        }

        assert!(machine.state().resources.returns_drained());
    }

    #[test]
    fn transition_accepts_the_declared_shutdown_order() {
        let mut machine = open_machine();
        for event in [
            GhostEvent::BeginClose,
            GhostEvent::CallsDrained,
            GhostEvent::ReturnsDrained,
            GhostEvent::AsyncDrained,
            GhostEvent::SubscriptionsDrained,
        ] {
            machine.apply(event).unwrap();
        }
        machine
            .apply(GhostEvent::HostDetached)
            .expect_err("registrations and callback gate are not detached");
    }

    #[test]
    fn trace_has_schema_and_ordered_events() {
        let mut machine = open_machine();
        machine.apply(GhostEvent::BeginClose).unwrap();
        let trace = machine.trace();
        assert_eq!(trace.schema_version, SCHEMA_VERSION);
        assert_eq!(trace.generation, 42);
        assert_eq!(trace.events[0], GhostEvent::BeginClose);
        assert!(!trace.trace_truncated);
        assert!(
            serde_json::to_string(&trace)
                .unwrap()
                .contains("beginClose")
        );
    }

    #[test]
    fn trace_marks_overflow_without_retaining_unbounded_history() {
        let mut machine = open_machine();
        for _ in 0..=MAX_TRACE_EVENTS {
            machine.apply(GhostEvent::RecordCleanupIssue).unwrap();
        }

        let trace = machine.trace();
        assert_eq!(trace.events.len(), MAX_TRACE_EVENTS);
        assert!(trace.trace_truncated);
    }

    #[test]
    fn fail_stop_is_terminal_for_late_instrumentation() {
        let mut machine = open_machine();
        machine.fail_stop(GhostFailure::BoundaryPanic).unwrap();
        assert!(!machine.active);
        assert!(matches!(
            machine.apply(GhostEvent::LeaveExternal),
            Err(GhostViolation::NoGeneration)
        ));
        assert_eq!(machine.trace().outcome, "fail_stopped");
    }

    #[test]
    fn complete_trace_reaches_closed_only_after_every_milestone() {
        let mut machine = open_machine();
        for event in [
            GhostEvent::StartDiagnostics,
            GhostEvent::BeginClose,
            GhostEvent::CallsDrained,
            GhostEvent::ReturnsDrained,
            GhostEvent::AsyncDrained,
            GhostEvent::SubscriptionsDrained,
            GhostEvent::CloseCallbackGate,
            GhostEvent::HostDetached,
            GhostEvent::ProveStateUnique,
            GhostEvent::ProveAddinQuiesced,
            GhostEvent::StateClosed,
            GhostEvent::HandlesDrained,
            GhostEvent::StopDiagnostics,
            GhostEvent::DiagnosticsDrained,
            GhostEvent::RtdDrained,
            GhostEvent::FinishClose,
        ] {
            machine.apply(event).unwrap();
        }
        machine.record_returned_success().unwrap();
        let trace = machine.trace();
        assert_eq!(trace.outcome, "returned_success");
        assert_eq!(machine.state().phase, GhostPhase::Closed);
    }
}
