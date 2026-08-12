import XlFnFormal.Lifecycle.Certificate

set_option autoImplicit false

namespace XlFnFormal.Composition

/-! The lifecycle marker says whether the current open generation has crossed
    the `finishOpen` commit point.  `none` is therefore deliberately about the
    current session, not about the historical value of `generation`. -/
structure State where
  lifecycle : Lifecycle.State
  currentShutdown : Option Shutdown.State
  deriving DecidableEq, Repr

namespace State

def ShutdownSessionLive : Shutdown.Phase → Prop
  | .open => True
  | .closing _ => True
  | .closed => False
  | .failStopped _ => True

def SessionConsistent (s : State) : Prop :=
  match s.lifecycle.phase, s.currentShutdown with
  | .closed, none => True
  | .opening, none => True
  | .openRollbackPending, none => True
  | .closing, none => True
  | .open, some shutdown => shutdown.phase = .open
  | .closing, some shutdown => ShutdownSessionLive shutdown.phase
  | .closed, some _ => False
  | .opening, some _ => False
  | .openRollbackPending, some _ => False
  | .open, none => False

def WellFormed (s : State) : Prop :=
  s.lifecycle.WellFormed ∧ s.SessionConsistent

def Valid (s : State) : Prop :=
  s.lifecycle.Valid ∧ s.SessionConsistent

def initialState : State :=
  { lifecycle := Lifecycle.State.initialState
    currentShutdown := none }

def committedOpenState : Shutdown.State :=
  Shutdown.State.opened {}

theorem initialState_wellFormed : initialState.WellFormed := by
  exact ⟨Lifecycle.State.initialState_wellFormed, by
    simp [initialState, SessionConsistent, Lifecycle.State.initialState]⟩

theorem initialState_valid : initialState.Valid := by
  exact ⟨Lifecycle.State.initialState_valid, by
    simp [initialState, SessionConsistent, Lifecycle.State.initialState]⟩

theorem committedOpenState_is_open :
    committedOpenState.phase = .open := by
  rfl

end State

end XlFnFormal.Composition
