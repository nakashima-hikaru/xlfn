import XlFnFormal.Composition.Invariant
import XlFnFormal.Shutdown.Milestones

set_option autoImplicit false

namespace XlFnFormal.Composition

/-! The three successful close paths expose the same resource-level safety
    result, but they differ in whether a committed Shutdown session exists. -/

theorem successful_committed_close_is_quiescent
    {s t : State}
    (hStep : Step s .finishCommittedShutdown t) :
    ∃ session,
      t.currentShutdown = some session ∧
      session.state.phase = .closed ∧
      session.state.resources.Quiescent := by
  cases hStep with
  | @finishCommittedShutdown session shutdown' hSession hPhase hNoAttempt hShutdown =>
      have hPost := Shutdown.Step.finishClose_postcondition hShutdown
      exact ⟨{ session with state := shutdown' }, rfl, hPost.1, hPost.2⟩

theorem successful_uncommitted_close_is_quiescent
    {s t : State} {resources : Shutdown.Resources}
    (hStep : Step s (.finishUncommittedFinalClose resources) t) :
    t.lifecycle.phase = .closed ∧ resources.Quiescent := by
  cases hStep with
  | finishUncommittedFinalClose hNoSession hPhase hNoAttempt hOwner hQuiescent =>
      exact ⟨rfl, hQuiescent⟩

theorem successful_open_rollback_is_quiescent
    {s t : State} {resources : Shutdown.Resources}
    (hStep : Step s (.finishOpenRollback resources) t) :
    t.lifecycle.phase = .closed ∧ resources.Quiescent := by
  cases hStep with
  | finishOpenRollback hNoSession hPhase hNoAttempt hOwner hQuiescent =>
      exact ⟨rfl, hQuiescent⟩

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

theorem successful_xlAutoClose_is_safe
    {initial final : State}
    (hInitial : initial.Valid)
    (hReachable : Reachable initial final)
    (hSafe : final.lifecycle.ReturnSafe) :
    final.currentShutdown = none := by
  exact returnSafe_has_no_active_shutdown
    (Reachable.valid hInitial hReachable) hSafe

end XlFnFormal.Composition
