import XlFnFormal.Shutdown.Model

set_option autoImplicit false

namespace XlFnFormal

/-! The logical lifecycle and the physical DLL lifetime are independent.

    `leaseHeld` is the self-reference acquired from the generated
    `xlAutoOpen` address. Logical terminal removal may publish `closed` while
    that lease remains held; release is admitted only after the quiescence
    certificate has been established. A quarantine keeps the lease by
    construction. -/

structure ResidencyState where
  logicalPhase : Shutdown.Phase
  leaseHeld : Bool
  removalComplete : Bool
  callbacksQuiescent : Bool
  generationQuiescent : Bool
  runtimeQuiescent : Bool
  deriving DecidableEq, Repr

def ResidencyState.Safe (s : ResidencyState) : Prop :=
  s.leaseHeld = true ∨
  (s.logicalPhase = .closed ∧
    s.callbacksQuiescent = true ∧
    s.generationQuiescent = true ∧
    s.runtimeQuiescent = true)

inductive ResidencyEvent where
  | autoCloseHint
  | terminalRemoval
  | quarantine (reason : Shutdown.Failure)
  | autoCloseAfterRemoval
  deriving DecidableEq, Repr

inductive ResidencyStep : ResidencyState → ResidencyEvent → ResidencyState → Prop where
  | autoCloseHint {s : ResidencyState} :
      ResidencyStep s .autoCloseHint s

  | terminalRemoval {s : ResidencyState}
      (hOpen : s.logicalPhase = .open)
      (hLease : s.leaseHeld = true)
      (hCallbacks : s.callbacksQuiescent = true)
      (hGeneration : s.generationQuiescent = true)
      (hRuntime : s.runtimeQuiescent = true) :
      ResidencyStep s .terminalRemoval
        { s with logicalPhase := .closed, removalComplete := true }

  | quarantine {s : ResidencyState} {reason : Shutdown.Failure}
      (hLive : s.logicalPhase = .open ∨
        ∃ stage, s.logicalPhase = .closing stage)
      (hLease : s.leaseHeld = true) :
      ResidencyStep s (.quarantine reason)
        { s with logicalPhase := .quarantined reason, removalComplete := false }

  | autoCloseAfterRemoval {s : ResidencyState}
      (hClosed : s.logicalPhase = .closed)
      (hRemoval : s.removalComplete = true)
      (hLease : s.leaseHeld = true)
      (hCallbacks : s.callbacksQuiescent = true)
      (hGeneration : s.generationQuiescent = true)
      (hRuntime : s.runtimeQuiescent = true) :
      ResidencyStep s .autoCloseAfterRemoval
        { s with leaseHeld := false, removalComplete := false }

theorem autoCloseHint_is_noop
    {s t : ResidencyState}
    (hStep : ResidencyStep s .autoCloseHint t) :
    t = s := by
  cases hStep
  rfl

theorem terminalRemoval_retains_lease
    {s t : ResidencyState}
    (hStep : ResidencyStep s .terminalRemoval t) :
    t.logicalPhase = .closed ∧
    t.leaseHeld = true ∧
    t.removalComplete = true := by
  cases hStep with
  | terminalRemoval hOpen hLease hCallbacks hGeneration hRuntime =>
      exact ⟨rfl, hLease, rfl⟩

theorem ResidencyStep.safe_preserved
    {s t : ResidencyState} {event : ResidencyEvent}
    (hSafe : s.Safe)
    (hStep : ResidencyStep s event t) :
    t.Safe := by
  cases hStep with
  | autoCloseHint => exact hSafe
  | terminalRemoval hOpen hLease hCallbacks hGeneration hRuntime =>
      simp [ResidencyState.Safe, hLease]
  | quarantine hLive hLease =>
      simp [ResidencyState.Safe, hLease]
  | autoCloseAfterRemoval hClosed hRemoval hLease hCallbacks hGeneration hRuntime =>
      simp [ResidencyState.Safe, hClosed, hCallbacks, hGeneration, hRuntime]

theorem autoCloseAfterRemoval_requires_quiescence
    {s t : ResidencyState}
    (hStep : ResidencyStep s .autoCloseAfterRemoval t) :
    s.logicalPhase = .closed ∧
    s.removalComplete = true ∧
    s.callbacksQuiescent = true ∧
    s.generationQuiescent = true ∧
    s.runtimeQuiescent = true := by
  cases hStep with
  | autoCloseAfterRemoval hClosed hRemoval hLease hCallbacks hGeneration hRuntime =>
      exact ⟨hClosed, hRemoval, hCallbacks, hGeneration, hRuntime⟩

theorem quarantine_retains_lease
    {s t : ResidencyState} {reason : Shutdown.Failure}
    (hStep : ResidencyStep s (.quarantine reason) t) :
    t.leaseHeld = true := by
  cases hStep with
  | quarantine hLive hLease => exact hLease

end XlFnFormal
