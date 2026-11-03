import XlFnFormal.Lifecycle.Certificate

set_option autoImplicit false

namespace XlFnFormal.Composition

/-! The lifecycle marker says whether the current open generation has crossed
    the `finishOpen` commit point.  `none` is therefore deliberately about the
    current session, not about the historical value of `generation`. -/
structure ShutdownSession where
  generation : Lifecycle.AttemptId
  state : Shutdown.State
  deriving DecidableEq, Repr

namespace ShutdownSession

def withState (session : ShutdownSession) (state : Shutdown.State) : ShutdownSession :=
  { session with state }

def closed (session : ShutdownSession) : ShutdownSession :=
  withState session { session.state with phase := .closed }

end ShutdownSession

structure State where
  lifecycle : Lifecycle.State
  currentShutdown : Option ShutdownSession
  /-- Ghost evidence that the runtime has established logical quiescence for the
      current lifecycle publication.  It is intentionally retained after a
      successful close so the return theorem does not depend on a retired
      Shutdown session remaining in memory. -/
  logicalQuiescenceCertified : Bool
  deriving DecidableEq, Repr

namespace State

def withShutdown (s : State) (shutdown : Option ShutdownSession) : State :=
  { s with currentShutdown := shutdown }

def ShutdownSessionPublished : Shutdown.Phase → Prop
  | .open => True
  | .closing _ => True
  | .closed => True
  | .quarantined _ => True
  | .failStopped _ => True

theorem shutdownSessionPublished_all (phase : Shutdown.Phase) :
    ShutdownSessionPublished phase := by
  cases phase <;> trivial

def shutdownPhaseQuarantined : Shutdown.Phase → Bool
  | .quarantined _ => true
  | _ => false

/-- Open records that a committed session was published, not that a later
    quarantine has completed ordinary cleanup. Only retention quarantine is
    allowed beside Open; general Closing/Closed shutdown targets stay excluded. -/
def OpenSessionCompatible (phase : Shutdown.Phase) : Prop :=
  phase = .open ∨ shutdownPhaseQuarantined phase = true

def SessionConsistent (s : State) : Prop :=
  match s.lifecycle.phase, s.currentShutdown with
  | .closed, none => True
  | .opening, none => True
  | .openRollbackPending, none => True
  | .closing, none => True
  | .quarantined, none => True
  | .quarantined, some _ => False
  | .open, some session => session.generation = s.lifecycle.generation ∧
      OpenSessionCompatible session.state.phase
  | .closing, some session =>
      s.lifecycle.openAttempt = none ∧
      session.generation = s.lifecycle.generation ∧
      ShutdownSessionPublished session.state.phase
  | .closed, some session =>
      session.generation = s.lifecycle.generation ∧
      session.state.phase = .closed ∧
      s.lifecycle.cleanupOwner = some .finalClose
  | .opening, some _ => False
  | .openRollbackPending, some _ => False
  | .open, none => False

def CurrentShutdownCertified (s : State) : Prop :=
  match s.currentShutdown with
  | none => True
  | some session => session.state.Certified

def WellFormed (s : State) : Prop :=
  s.lifecycle.WellFormed ∧
  s.SessionConsistent ∧
  s.CurrentShutdownCertified

def Valid (s : State) : Prop :=
  s.lifecycle.Valid ∧
  s.SessionConsistent ∧
  s.CurrentShutdownCertified

def LogicalCloseCertificationConsistent (s : State) : Prop :=
  s.logicalQuiescenceCertified = true ↔ s.lifecycle.phase = .closed

def hasQuarantinedShutdown (s : State) : Bool :=
  match s.currentShutdown with
  | some session => shutdownPhaseQuarantined session.state.phase
  | none => false

/-- A terminal retention outcome, never a successful-close certificate. Active
    cleanup owners remain in progress until their release is observed. -/
def TerminalQuarantine (s : State) : Prop :=
  s.logicalQuiescenceCertified = false ∧
  s.lifecycle.openAttempt = none ∧ s.lifecycle.cleanupOwner = none ∧
  ((s.lifecycle.phase = .quarantined ∧ s.currentShutdown = none) ∨
    ((s.lifecycle.phase = .open ∨ s.lifecycle.phase = .closing) ∧
      s.hasQuarantinedShutdown = true))

def initialState : State :=
  { lifecycle := Lifecycle.State.initialState
    currentShutdown := none
    logicalQuiescenceCertified := true }

theorem initialState_wellFormed : initialState.WellFormed := by
  exact ⟨Lifecycle.State.initialState_wellFormed, by
    simp [initialState, SessionConsistent, Lifecycle.State.initialState], by
    simp [initialState, CurrentShutdownCertified]⟩

theorem initialState_valid : initialState.Valid := by
  exact ⟨Lifecycle.State.initialState_valid, by
    simp [initialState, SessionConsistent, Lifecycle.State.initialState], by
    simp [initialState, CurrentShutdownCertified]⟩

theorem initialState_logicalCloseCertificationConsistent :
    initialState.LogicalCloseCertificationConsistent := by
  simp [initialState, LogicalCloseCertificationConsistent, Lifecycle.State.initialState]

theorem initialState_logicalQuiescenceCertified : initialState.logicalQuiescenceCertified = true := by
  rfl

theorem committed_session_open_or_quarantined
    {s : State} {session : ShutdownSession}
    (hConsistent : s.SessionConsistent)
    (hOpen : s.lifecycle.phase = .open)
    (hSession : s.currentShutdown = some session) :
    OpenSessionCompatible session.state.phase := by
  have h := hConsistent
  simp [SessionConsistent, hOpen, hSession] at h
  exact h.2

theorem committed_session_generation_matches
    {s : State} {session : ShutdownSession}
    (hConsistent : s.SessionConsistent)
    (hSession : s.currentShutdown = some session)
    (hPhase : s.lifecycle.phase = .open ∨ s.lifecycle.phase = .closing ∨
      s.lifecycle.phase = .closed) :
    session.generation = s.lifecycle.generation := by
  cases hPhase with
  | inl hOpen =>
      have h := hConsistent
      simp [SessionConsistent, hOpen, hSession] at h
      exact h.1
  | inr hRest =>
      cases hRest with
      | inl hClosing =>
          have h := hConsistent
          simp [SessionConsistent, hClosing, hSession] at h
          exact h.2.1
      | inr hClosed =>
          have h := hConsistent
          simp [SessionConsistent, hClosed, hSession] at h
          exact h.1

end State

end XlFnFormal.Composition
