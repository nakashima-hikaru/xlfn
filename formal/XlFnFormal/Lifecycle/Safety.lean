import XlFnFormal.Lifecycle.Invariant

set_option autoImplicit false

namespace XlFnFormal.Lifecycle

theorem requestFinalClose_invalidates_sampled_epoch
    {s t : State}
    (hStep : Step s .requestFinalClose t) :
    ¬ t.CanBeginOpen s.closeEpoch := by
  cases hStep
  simp_all [State.CanBeginOpen, phaseAfterFinalClose]

theorem opening_rejects_second_beginOpen
    {s t : State} {sampledEpoch : Epoch} {attempt : AttemptId}
    (hOpening : s.phase = .opening) :
    ¬ Step s (.beginOpen sampledEpoch attempt) t := by
  intro hStep
  cases hStep <;> simp_all [State.CanBeginOpen]

theorem closing_cannot_transition_to_open
    {s t : State} {event : Event}
    (hClosing : s.phase = .closing)
    (hStep : Step s event t) :
    t.phase ≠ .open := by
  intro hOpen
  cases hStep <;>
    (try cases hSrcPhase : s.phase) <;>
    simp_all [phaseAfterFinalClose]

theorem closing_attempt_cannot_commit_open
    {s t : State} {attempt : AttemptId}
    (hClosing : s.phase = .closing) :
    ¬ Step s (.finishOpen attempt) t := by
  intro hStep
  cases hStep <;> simp_all

theorem rollback_finish_cannot_reopen
    {s t : State}
    (hStep : Step s .finishOpenRollback t) :
    t.phase ≠ .open := by
  intro hOpen
  cases hStep <;> simp_all

theorem target_open_requires_finishOpen
    {s t : State} {event : Event}
    (hStep : Step s event t)
    (hOpen : t.phase = .open) :
    ∃ attempt, event = .finishOpen attempt := by
  cases hSourcePhase : s.phase <;>
    cases hStep <;>
    simp_all [phaseAfterFinalClose]

theorem finishOpen_sets_generation
    {s t : State} {attempt : AttemptId}
    (hStep : Step s (.finishOpen attempt) t) :
    t.phase = .open ∧
    t.generation = attempt ∧
    t.openAttempt = none := by
  cases hStep <;> simp_all

theorem cleanup_owner_excludes_open_attempt
    {s : State}
    (hWF : s.WellFormed) :
    s.openAttempt.isSome → s.cleanupOwner.isNone :=
  hWF.1

theorem finish_close_publishes_closed_but_owner_remains
    {s t : State}
    (hStep : Step s .finishFinalClose t) :
    t.phase = .closed ∧
    t.cleanupOwner = some .finalClose := by
  cases hStep with
  | finishFinalClose _ _ hOwner =>
      exact ⟨rfl, hOwner⟩

theorem finish_open_rollback_publishes_closed_but_owner_remains
    {s t : State}
    (hStep : Step s .finishOpenRollback t) :
    t.phase = .closed ∧
    t.cleanupOwner = some .openRollback := by
  cases hStep with
  | finishOpenRollback _ _ hOwner =>
      exact ⟨rfl, hOwner⟩

theorem closed_becomes_return_safe_after_owner_release
    {s t u : State}
    (hFinish : Step s .finishFinalClose t)
    (hRelease : Step t .releaseCleanupOwner u) :
    u.ReturnSafe := by
  cases hFinish <;> cases hRelease <;>
    simp_all [State.ReturnSafe]

theorem rollback_closed_becomes_return_safe_after_owner_release
    {s t u : State}
    (hFinish : Step s .finishOpenRollback t)
    (hRelease : Step t .releaseCleanupOwner u) :
    u.ReturnSafe := by
  cases hFinish <;> cases hRelease <;>
    simp_all [State.ReturnSafe]

theorem closing_without_owner_can_acquire_owner
    {s t : State}
    (hStep : Step s .acquireFinalCloseOwner t) :
    t.cleanupOwner = some .finalClose := by
  cases hStep
  rfl

end XlFnFormal.Lifecycle
