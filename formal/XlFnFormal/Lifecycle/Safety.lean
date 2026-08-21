import XlFnFormal.Lifecycle.Invariant

set_option autoImplicit false

namespace XlFnFormal.Lifecycle

theorem Step.requestFinalClose_changes_epoch
    {s t : State}
    (hStep : Step s .requestFinalClose t) :
    t.closeEpoch = s.closeEpoch + 1 := by
  cases hStep
  rfl

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

theorem finish_removal_publishes_closed_but_owner_remains
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

theorem acquireFinalCloseOwner_enabled
    {s : State}
    (hPhase : s.phase = .closing)
    (hNoAttempt : s.openAttempt = none)
    (hNoOwner : s.cleanupOwner = none) :
    ∃ t, Step s .acquireFinalCloseOwner t := by
  exact ⟨{ s with cleanupOwner := some .finalClose },
    Step.acquireFinalCloseOwner hPhase hNoAttempt hNoOwner⟩

theorem acquireOpenRollbackOwner_enabled
    {s : State}
    (hPhase : s.phase = .openRollbackPending)
    (hNoAttempt : s.openAttempt = none)
    (hNoOwner : s.cleanupOwner = none) :
    ∃ t, Step s .acquireOpenRollbackOwner t := by
  exact ⟨{ s with cleanupOwner := some .openRollback },
    Step.acquireOpenRollbackOwner hPhase hNoAttempt hNoOwner⟩

theorem Step.generation_change_requires_finishOpen
    {s t : State} {event : Event}
    (hStep : Step s event t)
    (hChanged : t.generation ≠ s.generation) :
    ∃ attempt,
      event = .finishOpen attempt ∧
      t.generation = attempt := by
  cases hStep <;> simp_all

theorem Steps.open_target_requires_finishOpen
    {s t : State} {events : List Event}
    (hNotOpen : s.phase ≠ .open)
    (hSteps : Steps s events t)
    (hOpen : t.phase = .open) :
    ∃ attempt, .finishOpen attempt ∈ events := by
  induction hSteps with
  | refl =>
      exact False.elim (hNotOpen hOpen)
  | @cons source middle target event tail hStep hTail ih =>
      by_cases hMiddleOpen : middle.phase = .open
      · have hOrigin := target_open_requires_finishOpen hStep hMiddleOpen
        rcases hOrigin with ⟨attempt, hEvent⟩
        exact ⟨attempt, by simp [hEvent]⟩
      · have hTailOrigin := ih hMiddleOpen hOpen
        rcases hTailOrigin with ⟨attempt, hEvent⟩
        exact ⟨attempt, by simp [hEvent]⟩

theorem Steps.open_after_closed_requires_finishOpen
    {s t : State} {events : List Event}
    (hClosed : s.phase = .closed)
    (hSteps : Steps s events t)
    (hOpen : t.phase = .open) :
    ∃ attempt, .finishOpen attempt ∈ events :=
  Steps.open_target_requires_finishOpen (by simp [hClosed]) hSteps hOpen

theorem Steps.returnSafe_has_no_active_owner
    {s t : State} {events : List Event}
    (_hSteps : Steps s events t)
    (hSafe : t.ReturnSafe) :
    t.cleanupOwner = none :=
  hSafe.2.2

end XlFnFormal.Lifecycle
