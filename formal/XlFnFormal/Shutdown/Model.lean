import Std

set_option autoImplicit false

namespace XlFnFormal.Shutdown

/-! The abstract shutdown machine deliberately starts after a successful open.
    Opening and rollback are implementation states; they are represented by
    the Rust boundary certificates rather than by a second abstract protocol. -/

inductive CloseStage where
  | drainCalls
  | drainReturns
  | drainAsync
  | stopSubscriptions
  | detachHost
  | closeState
  | drainHandles
  | stopDiagnostics
  | drainRtd
  | finalize
  deriving DecidableEq, Repr

namespace CloseStage

def rank : CloseStage → Nat
  | .drainCalls => 1
  | .drainReturns => 2
  | .drainAsync => 3
  | .stopSubscriptions => 4
  | .detachHost => 5
  | .closeState => 6
  | .drainHandles => 7
  | .stopDiagnostics => 8
  | .drainRtd => 9
  | .finalize => 10

end CloseStage

inductive Failure where
  | boundaryPanic
  | unregisterFailed
  | returnShutdownFailed
  | asyncShutdownFailed
  | rtdShutdownFailed
  | handleShutdownFailed
  | stateEscaped
  | addinShutdownFailed
  | diagnosticsShutdownFailed
  | invariantViolation
  deriving DecidableEq, Repr

inductive Phase where
  | open
  | closing (stage : CloseStage)
  | closed
  | failStopped (reason : Failure)
  deriving DecidableEq, Repr

namespace Phase

def rank : Phase → Nat
  | .open => 0
  | .closing stage => stage.rank
  | .closed => 11
  | .failStopped _ => 11

theorem rank_le_terminal (phase : Phase) : phase.rank ≤ 11 := by
  cases phase with
  | «open» => simp [rank]
  | closing stage => cases stage <;> simp [rank, CloseStage.rank]
  | closed => simp [rank]
  | failStopped reason => simp [rank]

def IsLive : Phase → Prop
  | .open => True
  | .closing _ => True
  | .closed => False
  | .failStopped _ => False

def AllowsReturnCreation : Phase → Prop
  | .open => True
  | .closing .drainCalls => True
  | _ => False

def AllowsReturnFree : Phase → Prop
  | .open => True
  | .closing .drainCalls => True
  | .closing .drainReturns => True
  | _ => False

def AllowsAsyncCreation : Phase → Prop
  | .open => True
  | .closing .drainCalls => True
  | .closing .drainReturns => True
  | .closing .drainAsync => True
  | _ => False

def AllowsSubscriptionCreation : Phase → Prop
  | .open => True
  | .closing .drainCalls => True
  | .closing .drainReturns => True
  | .closing .drainAsync => True
  | .closing .stopSubscriptions => True
  | _ => False

def AllowsRtdCreation : Phase → Prop
  | .open => True
  | .closing .drainCalls => True
  | .closing .drainReturns => True
  | .closing .drainAsync => True
  | .closing .stopSubscriptions => True
  | .closing .detachHost => True
  | .closing .closeState => True
  | .closing .drainHandles => True
  | .closing .stopDiagnostics => True
  | .closing .drainRtd => True
  | _ => False

def AllowsHandleCreation : Phase → Prop
  | .open => True
  | .closing .drainCalls => True
  | .closing .drainReturns => True
  | .closing .drainAsync => True
  | .closing .stopSubscriptions => True
  | .closing .detachHost => True
  | .closing .closeState => True
  | .closing .drainHandles => True
  | _ => False

def AllowsDiagnosticCreation : Phase → Prop
  | .open => True
  | .closing .drainCalls => True
  | .closing .drainReturns => True
  | .closing .drainAsync => True
  | .closing .stopSubscriptions => True
  | .closing .detachHost => True
  | .closing .closeState => True
  | .closing .drainHandles => True
  | .closing .stopDiagnostics => True
  | _ => False

end Phase

/-! Resource fields are restricted to evidence the framework can observe or
    receive as an explicit Add-in contract.  Arbitrary user threads and native
    callbacks are represented by `addinQuiesced`, not by unverifiable ghost
    counters. -/
structure Resources where
  ingressOpen : Bool := true
  externalEntries : Nat := 0
  registrations : Nat := 0
  eventRegistrations : Nat := 0
  registrationStateKnown : Bool := true
  callbackGateOpen : Bool := true
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
  stateUnique : Bool := false
  addinQuiesced : Bool := false
  stateOwnedByRuntime : Bool := true
  diagnosticsPending : Nat := 0
  diagnosticsRunning : Bool := false
  cleanupIssues : Nat := 0
  deriving DecidableEq, Repr

namespace Resources

def HostDetached (r : Resources) : Prop :=
  r.ingressOpen = false ∧
  r.registrations = 0 ∧
  r.eventRegistrations = 0 ∧
  r.registrationStateKnown = true ∧
  r.callbackGateOpen = false

def CallsDrained (r : Resources) : Prop :=
  r.externalEntries = 0 ∧ r.activeCalls = 0

def ReturnsDrained (r : Resources) : Prop :=
  r.returnBlocks = 0 ∧ r.returnFreeOperations = 0

def AsyncDrained (r : Resources) : Prop :=
  r.asyncTasks = 0 ∧ r.asyncExecutorRunning = false

def SubscriptionsDrained (r : Resources) : Prop :=
  r.subscriptions = 0 ∧ r.callbacks = 0

def RtdDrained (r : Resources) : Prop :=
  r.rtdOperations = 0 ∧
  r.rtdClassFactories = 0 ∧
  r.rtdServers = 0 ∧
  r.rtdServerLocks = 0

def HandlesDrained (r : Resources) : Prop :=
  r.handleOperations = 0 ∧ r.handles = 0

def StateClosed (r : Resources) : Prop :=
  r.stateUnique = true ∧
  r.addinQuiesced = true ∧
  r.stateOwnedByRuntime = false

def DiagnosticsDrained (r : Resources) : Prop :=
  r.diagnosticsPending = 0 ∧ r.diagnosticsRunning = false

def Quiescent (r : Resources) : Prop :=
  r.HostDetached ∧
  r.CallsDrained ∧
  r.ReturnsDrained ∧
  r.AsyncDrained ∧
  r.SubscriptionsDrained ∧
  r.RtdDrained ∧
  r.HandlesDrained ∧
  r.StateClosed ∧
  r.DiagnosticsDrained

def CleanupComplete (r : Resources) : Prop :=
  r.cleanupIssues = 0

def ProducerAlive (r : Resources) : Prop :=
  r.externalEntries > 0 ∨
  r.activeCalls > 0 ∨
  r.returnFreeOperations > 0 ∨
  r.asyncTasks > 0 ∨
  r.rtdOperations > 0 ∨
  r.subscriptions > 0 ∨
  r.callbacks > 0 ∨
  r.handleOperations > 0 ∨
  r.diagnosticsPending > 0

theorem quiescent_hostDetached {r : Resources}
    (h : r.Quiescent) : r.HostDetached := h.1

theorem quiescent_callsDrained {r : Resources}
    (h : r.Quiescent) : r.CallsDrained := h.2.1

theorem quiescent_returnsDrained {r : Resources}
    (h : r.Quiescent) : r.ReturnsDrained := h.2.2.1

theorem quiescent_asyncDrained {r : Resources}
    (h : r.Quiescent) : r.AsyncDrained := h.2.2.2.1

theorem quiescent_subscriptionsDrained {r : Resources}
    (h : r.Quiescent) : r.SubscriptionsDrained := h.2.2.2.2.1

theorem quiescent_rtdDrained {r : Resources}
    (h : r.Quiescent) : r.RtdDrained := h.2.2.2.2.2.1

theorem quiescent_handlesDrained {r : Resources}
    (h : r.Quiescent) : r.HandlesDrained := h.2.2.2.2.2.2.1

theorem quiescent_stateClosed {r : Resources}
    (h : r.Quiescent) : r.StateClosed := h.2.2.2.2.2.2.2.1

theorem quiescent_diagnosticsDrained {r : Resources}
    (h : r.Quiescent) : r.DiagnosticsDrained := h.2.2.2.2.2.2.2.2

end Resources

structure State where
  phase : Phase
  resources : Resources
  deriving DecidableEq, Repr

namespace State

def opened (resources : Resources) : State :=
  { phase := .open, resources }

def Successful (s : State) : Prop :=
  s.phase = .closed

def Quiescent (s : State) : Prop :=
  s.resources.Quiescent

def ClosedClean (s : State) : Prop :=
  s.Successful ∧ s.Quiescent ∧ s.resources.CleanupComplete

def ClosedDegraded (s : State) : Prop :=
  s.Successful ∧ s.Quiescent ∧ ¬s.resources.CleanupComplete

end State

end XlFnFormal.Shutdown
