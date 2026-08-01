import Std

set_option autoImplicit false

namespace ExcelXllFormal.Shutdown

/-- Ordered shutdown stages.  The constructor names describe the subsystem
that must be made quiescent before advancing to the next stage. -/
inductive CloseStage where
  | detachHost
  | drainCalls
  | drainReturns
  | drainAsync
  | drainRtd
  | drainHandles
  | closeState
  | stopDiagnostics
  | finalize
  deriving DecidableEq, Repr

namespace CloseStage

/-- A numerical progress measure used only for monotonicity proofs. -/
def rank : CloseStage → Nat
  | .detachHost => 1
  | .drainCalls => 2
  | .drainReturns => 3
  | .drainAsync => 4
  | .drainRtd => 5
  | .drainHandles => 6
  | .closeState => 7
  | .stopDiagnostics => 8
  | .finalize => 9

end CloseStage

/-- Reasons for deliberately refusing to return successfully from shutdown.
`failStopped` models process termination or an equivalent host-level action
that prevents the XLL from being unloaded while code may still execute. -/
inductive Failure where
  | boundaryPanic
  | unregisterFailed
  | returnShutdownFailed
  | asyncShutdownFailed
  | rtdShutdownFailed
  | handleShutdownFailed
  | stateEscaped
  | addinShutdownFailed
  | workerShutdownFailed
  | diagnosticsShutdownFailed
  | invariantViolation
  deriving DecidableEq, Repr

/-- Runtime lifecycle phase.  `closed` is the only successful terminal phase. -/
inductive Phase where
  | open
  | closing (stage : CloseStage)
  | closed
  | failStopped (reason : Failure)
  deriving DecidableEq, Repr

namespace Phase

/-- Progress never decreases along a certified transition. -/
def rank : Phase → Nat
  | .open => 0
  | .closing stage => stage.rank
  | .closed => 10
  | .failStopped _ => 10

/-- Every phase rank is bounded by the two terminal phases. -/
theorem rank_le_terminal (phase : Phase) : phase.rank ≤ 10 := by
  cases phase with
  | «open» => simp [rank]
  | closing stage =>
      cases stage <;> simp [rank, CloseStage.rank]
  | closed => simp [rank]
  | failStopped reason => simp [rank]

/-- Code is permitted to perform ordinary completion/cleanup transitions only
while the runtime is open or is executing the close protocol. -/
def IsLive : Phase → Prop
  | .open => True
  | .closing _ => True
  | .closed => False
  | .failStopped _ => False

/-- DLL-owned worksheet return blocks can be created only by calls that were
admitted before the call-drain milestone. -/
def AllowsReturnCreation : Phase → Prop
  | .open => True
  | .closing .detachHost => True
  | .closing .drainCalls => True
  | _ => False

/-- Excel may enter `xlAutoFree12` after the producing UDF has returned.  The
free callback remains admissible until the dedicated return-drain milestone. -/
def AllowsReturnFree : Phase → Prop
  | .open => True
  | .closing .detachHost => True
  | .closing .drainCalls => True
  | .closing .drainReturns => True
  | _ => False

/-- Async tasks may still be created by calls or tasks admitted before close
began, but not after the async-drain milestone has completed. -/
def AllowsAsyncCreation : Phase → Prop
  | .open => True
  | .closing .detachHost => True
  | .closing .drainCalls => True
  | .closing .drainReturns => True
  | .closing .drainAsync => True
  | _ => False

/-- RTD activity may continue until the RTD drain is linearized. -/
def AllowsRtdCreation : Phase → Prop
  | .open => True
  | .closing .detachHost => True
  | .closing .drainCalls => True
  | .closing .drainReturns => True
  | .closing .drainAsync => True
  | .closing .drainRtd => True
  | _ => False

/-- Handle activity may continue until the handle drain is linearized. -/
def AllowsHandleCreation : Phase → Prop
  | .open => True
  | .closing .detachHost => True
  | .closing .drainCalls => True
  | .closing .drainReturns => True
  | .closing .drainAsync => True
  | .closing .drainRtd => True
  | .closing .drainHandles => True
  | _ => False

/-- Add-in-owned resources may be manipulated until `closeState` succeeds. -/
def AllowsStateResourceCreation : Phase → Prop
  | .open => True
  | .closing .detachHost => True
  | .closing .drainCalls => True
  | .closing .drainReturns => True
  | .closing .drainAsync => True
  | .closing .drainRtd => True
  | .closing .drainHandles => True
  | .closing .closeState => True
  | _ => False

end Phase

/-- Abstract resource inventory relevant to safe XLL unloading.

The model intentionally records counts rather than implementation containers.
A Rust refinement must show that each count is the abstraction of the
corresponding registry, in-flight guard, executor, COM object, or worker. -/
structure Resources where
  registrations : Nat := 0
  eventRegistrations : Nat := 0
  activeCalls : Nat := 0
  returnBlocks : Nat := 0
  returnFreeOperations : Nat := 0
  asyncTasks : Nat := 0
  asyncExecutorRunning : Bool := false
  rtdOperations : Nat := 0
  subscriptions : Nat := 0
  callbacks : Nat := 0
  rtdClassFactories : Nat := 0
  rtdServers : Nat := 0
  rtdServerLocks : Nat := 0
  handleOperations : Nat := 0
  handles : Nat := 0
  externalStateLeases : Nat := 0
  workers : Nat := 0
  workerJobs : Nat := 0
  addinResources : Nat := 0
  stateOwnedByRuntime : Bool := true
  diagnosticsPending : Nat := 0
  diagnosticsRunning : Bool := false
  /-- Best-effort disposal debt. This is deliberately excluded from
  `Quiescent`: it cannot make unloaded XLL code executable. -/
  cleanupIssues : Nat := 0
  deriving DecidableEq, Repr

namespace Resources

/-- No Excel-visible function/event registration points at this module. -/
def HostDetached (r : Resources) : Prop :=
  r.registrations = 0 ∧ r.eventRegistrations = 0

/-- Every synchronous framework call has released its `CallGuard`. -/
def CallsDrained (r : Resources) : Prop :=
  r.activeCalls = 0

/-- Excel owns no pending DLL-free return block and no `xlAutoFree12`
callback is executing. -/
def ReturnsDrained (r : Resources) : Prop :=
  r.returnBlocks = 0 ∧ r.returnFreeOperations = 0

/-- The async registry is empty and its executor has been joined. -/
def AsyncDrained (r : Resources) : Prop :=
  r.asyncTasks = 0 ∧ r.asyncExecutorRunning = false

/-- No RTD operation, subscription, callback, class factory, COM server, or
server lock remains live. -/
def RtdDrained (r : Resources) : Prop :=
  r.rtdOperations = 0 ∧
  r.subscriptions = 0 ∧
  r.callbacks = 0 ∧
  r.rtdClassFactories = 0 ∧
  r.rtdServers = 0 ∧
  r.rtdServerLocks = 0

/-- No handle operation or published handle value remains live. -/
def HandlesDrained (r : Resources) : Prop :=
  r.handleOperations = 0 ∧ r.handles = 0

/-- The Add-in state has no escaped lease, worker, queued/running worker job,
or other Add-in-owned resource, and the runtime root has been consumed. -/
def StateClosed (r : Resources) : Prop :=
  r.externalStateLeases = 0 ∧
  r.workers = 0 ∧
  r.workerJobs = 0 ∧
  r.addinResources = 0 ∧
  r.stateOwnedByRuntime = false

/-- The diagnostic queue is empty and its dispatcher has been joined. -/
def DiagnosticsDrained (r : Resources) : Prop :=
  r.diagnosticsPending = 0 ∧ r.diagnosticsRunning = false

/-- Exact successful-unload condition. -/
def Quiescent (r : Resources) : Prop :=
  r.HostDetached ∧
  r.CallsDrained ∧
  r.ReturnsDrained ∧
  r.AsyncDrained ∧
  r.RtdDrained ∧
  r.HandlesDrained ∧
  r.StateClosed ∧
  r.DiagnosticsDrained

/-- Operational cleanliness is tracked separately from unload safety. -/
def CleanupComplete (r : Resources) : Prop :=
  r.cleanupIssues = 0

/-- There is framework or user code that may still create subordinate
resources.  This is used to prevent resources from appearing spontaneously. -/
def ProducerAlive (r : Resources) : Prop :=
  r.activeCalls > 0 ∨
  r.returnFreeOperations > 0 ∨
  r.asyncTasks > 0 ∨
  r.rtdOperations > 0 ∨
  r.subscriptions > 0 ∨
  r.callbacks > 0 ∨
  r.handleOperations > 0 ∨
  r.externalStateLeases > 0 ∨
  r.workers > 0 ∨
  r.workerJobs > 0 ∨
  r.addinResources > 0

/-- Projections from the exact successful-unload condition.  Keeping these
lemmas next to the definition avoids brittle nested-conjunction projections in
client proofs. -/
theorem quiescent_hostDetached {r : Resources}
    (h : r.Quiescent) : r.HostDetached :=
  h.1

theorem quiescent_callsDrained {r : Resources}
    (h : r.Quiescent) : r.CallsDrained :=
  h.2.1

theorem quiescent_returnsDrained {r : Resources}
    (h : r.Quiescent) : r.ReturnsDrained :=
  h.2.2.1

theorem quiescent_asyncDrained {r : Resources}
    (h : r.Quiescent) : r.AsyncDrained :=
  h.2.2.2.1

theorem quiescent_rtdDrained {r : Resources}
    (h : r.Quiescent) : r.RtdDrained :=
  h.2.2.2.2.1

theorem quiescent_handlesDrained {r : Resources}
    (h : r.Quiescent) : r.HandlesDrained :=
  h.2.2.2.2.2.1

theorem quiescent_stateClosed {r : Resources}
    (h : r.Quiescent) : r.StateClosed :=
  h.2.2.2.2.2.2.1

theorem quiescent_diagnosticsDrained {r : Resources}
    (h : r.Quiescent) : r.DiagnosticsDrained :=
  h.2.2.2.2.2.2.2

end Resources

structure State where
  phase : Phase
  resources : Resources
  deriving DecidableEq, Repr

namespace State

/-- State expected immediately after successful `xlAutoOpen`.  The exact
resource inventory is supplied by the implementation abstraction. -/
def opened (resources : Resources) : State :=
  { phase := .open, resources }

/-- Successful terminal state. -/
def Successful (s : State) : Prop :=
  s.phase = .closed

/-- A successful state is safe to unload only when this proposition holds. -/
def Quiescent (s : State) : Prop :=
  s.resources.Quiescent

/-- Safe unload with no recorded best-effort cleanup debt. -/
def ClosedClean (s : State) : Prop :=
  s.Successful ∧ s.Quiescent ∧ s.resources.CleanupComplete

/-- Safe unload where disposal or metadata cleanup reported debt. -/
def ClosedDegraded (s : State) : Prop :=
  s.Successful ∧ s.Quiescent ∧ ¬s.resources.CleanupComplete

end State

end ExcelXllFormal.Shutdown
