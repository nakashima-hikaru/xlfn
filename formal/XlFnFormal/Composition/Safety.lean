import XlFnFormal.Composition.Invariant
import XlFnFormal.Shutdown.Milestones

set_option autoImplicit false

namespace XlFnFormal.Composition

/-! The three successful removal paths expose the same resource-level safety
    result, but they differ in whether a committed Shutdown session exists. -/

theorem successful_committed_close_is_quiescent
    {s t : State}
    (hStep : Step s .finishCommittedShutdown t) :
    ∃ session,
      t.currentShutdown = some session ∧
    session.state.phase = .closed ∧
    session.state.resources.Quiescent := by
  cases hStep with
  | @finishCommittedShutdown session hSession hCertificate =>
      exact ⟨ShutdownSession.closed session, rfl,
        rfl, hCertificate.shutdown_ready.2⟩

theorem successful_uncommitted_close_is_quiescent
    {s t : State} {resources : Shutdown.Resources}
    (hStep : Step s (.finishUncommittedFinalClose resources) t) :
    t.lifecycle.phase = .closed ∧ resources.Quiescent := by
  cases hStep with
  | finishUncommittedFinalClose hNoSession hCertificate =>
      exact ⟨rfl, hCertificate.resources_quiescent⟩

theorem successful_open_rollback_is_quiescent
    {s t : State} {resources : Shutdown.Resources}
    (hStep : Step s (.finishOpenRollback resources) t) :
    t.lifecycle.phase = .closed ∧ resources.Quiescent := by
  cases hStep with
  | finishOpenRollback hNoSession hCertificate =>
      exact ⟨rfl, hCertificate.resources_quiescent⟩

theorem published_closed_session_has_final_close_owner
    {s t : State}
    (hStep : Step s .publishCommittedClosed t) :
    t.lifecycle.phase = .closed ∧
    t.lifecycle.cleanupOwner = some .finalClose ∧
    ∃ session, t.currentShutdown = some session ∧ session.state.phase = .closed := by
  cases hStep with
  | @publishCommittedClosed session hSession hPhase hNoAttempt hOwner hShutdownClosed =>
      exact ⟨rfl, hOwner, session, hSession, hShutdownClosed⟩

theorem retired_closed_session_has_no_active_shutdown
    {s t : State}
    (hStep : Step s .retireCommittedShutdown t) :
    t.currentShutdown = none := by
  cases hStep
  rfl

theorem returnSafe_has_no_active_shutdown
    {s : State}
    (hValid : s.Valid)
    (hSafe : s.lifecycle.ReturnSafe) :
    s.currentShutdown = none := by
  cases hSession : s.currentShutdown with
  | none =>
      rfl
  | some session =>
      exfalso
      have hConsistent := hValid.2.1
      simp [State.SessionConsistent, hSafe.1, hSafe.2.2, hSession] at hConsistent

theorem returnSafe_reachable_state_has_no_active_shutdown
    {initial final : State}
    (hInitial : initial.Valid)
    (hReachable : Reachable initial final)
    (hSafe : final.lifecycle.ReturnSafe) :
    final.currentShutdown = none := by
  exact returnSafe_has_no_active_shutdown
    (Reachable.valid hInitial hReachable) hSafe

theorem reachable_returnSafe_is_logicalQuiescenceCertified
    {final : State}
    (hReachable : Reachable State.initialState final)
    (hSafe : final.lifecycle.ReturnSafe) :
    final.logicalQuiescenceCertified = true := by
  have hConsistent := Reachable.unloadCertificationConsistent
    State.initialState_unloadCertificationConsistent hReachable
  have hPhase := hSafe.1
  have h := hConsistent
  simp [State.UnloadCertificationConsistent, hPhase] at h
  exact h

/-- A successful abstract `xlAutoRemove` return is represented by a reachable
    state whose lifecycle has reached the return-safe point.  The ghost fact
    is retained in the result even after the committed Shutdown session is
    retired. -/
def SuccessfulReturn (final : State) : Prop :=
  Reachable State.initialState final ∧ final.lifecycle.ReturnSafe

theorem successful_xlAutoRemove_is_safe
    {final : State}
    (hSuccess : SuccessfulReturn final) :
    final.lifecycle.ReturnSafe ∧
    final.currentShutdown = none ∧
    final.logicalQuiescenceCertified = true := by
  rcases hSuccess with ⟨hReachable, hSafe⟩
  have hNoShutdown := returnSafe_reachable_state_has_no_active_shutdown
    State.initialState_valid hReachable hSafe
  have hCertified := reachable_returnSafe_is_logicalQuiescenceCertified
    hReachable hSafe
  exact ⟨hSafe, hNoShutdown, hCertified⟩

end XlFnFormal.Composition
