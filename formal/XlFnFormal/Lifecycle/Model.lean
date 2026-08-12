import Std

set_option autoImplicit false

namespace XlFnFormal.Lifecycle

/-! These are logical, non-wrapping counters.  The Rust bridge must either
    establish that the corresponding `u64` operation cannot wrap or turn an
    overflow into a fail-stop before emitting an abstract transition. -/
abbrev AttemptId := Nat
abbrev Epoch := Nat

inductive Phase where
  | closed
  | opening
  | open
  | closing
  | openRollbackPending
  deriving DecidableEq, Repr

inductive CleanupOwner where
  | finalClose
  | openRollback
  deriving DecidableEq, Repr

structure State where
  phase : Phase
  closeEpoch : Epoch
  openAttempt : Option AttemptId
  cleanupOwner : Option CleanupOwner
  generation : AttemptId
  deriving DecidableEq, Repr

namespace State

/-! `closed` is a publication state, not by itself a safe callback return. -/
def ReturnSafe (s : State) : Prop :=
  s.phase = .closed ∧
  s.openAttempt = none ∧
  s.cleanupOwner = none

def CanBeginOpen (s : State) (sampledEpoch : Epoch) : Prop :=
  s.phase = .closed ∧
  s.openAttempt = none ∧
  s.cleanupOwner = none ∧
  sampledEpoch = s.closeEpoch

def AttemptOwnerDisjoint (s : State) : Prop :=
  s.openAttempt.isSome → s.cleanupOwner.isNone

def PhaseConsistent (s : State) : Prop :=
  match s.phase with
  | .opening =>
      s.openAttempt.isSome ∧ s.cleanupOwner.isNone
  | .open =>
      s.openAttempt.isNone ∧ s.cleanupOwner.isNone
  | .openRollbackPending =>
      s.openAttempt.isNone
  | .closed =>
      s.openAttempt.isNone
  | .closing =>
      True

def OwnerConsistent (s : State) : Prop :=
  match s.cleanupOwner with
  | none => True
  | some .finalClose =>
      s.phase = .closing ∨ s.phase = .closed
  | some .openRollback =>
      s.phase = .openRollbackPending ∨
      s.phase = .closing ∨
      s.phase = .closed

def WellFormed (s : State) : Prop :=
  s.AttemptOwnerDisjoint ∧
  s.PhaseConsistent ∧
  s.OwnerConsistent

def IdentifierValid (s : State) : Prop :=
  (∀ id, s.openAttempt = some id → id ≠ 0) ∧
  (s.phase = .open → s.generation ≠ 0)

def Valid (s : State) : Prop :=
  s.WellFormed ∧ s.IdentifierValid

def initialState : State :=
  { phase := .closed
    closeEpoch := 0
    openAttempt := none
    cleanupOwner := none
    generation := 0 }

theorem initialState_wellFormed : initialState.WellFormed := by
  simp [initialState, WellFormed, AttemptOwnerDisjoint,
    PhaseConsistent, OwnerConsistent]

theorem initialState_identifierValid : initialState.IdentifierValid := by
  simp [initialState, IdentifierValid]

theorem initialState_valid : initialState.Valid := by
  exact ⟨initialState_wellFormed, initialState_identifierValid⟩

end State

end XlFnFormal.Lifecycle
