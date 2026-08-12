import XlFnFormal.Lifecycle.Safety

set_option autoImplicit false

namespace XlFnFormal.Lifecycle

def closedForCounterexample : State :=
  { phase := .closed
    closeEpoch := 1
    openAttempt := none
    cleanupOwner := none
    generation := 1 }

def unsafeBeginOpenIgnoringEpoch (s : State) (attempt : AttemptId) : State :=
  { s with phase := .opening, openAttempt := some attempt }

/-! If the epoch comparison is removed, an open that sampled the old epoch can
    resurrect after a close on an otherwise closed runtime. -/
theorem stale_epoch_can_resurrect_open :
    let s := closedForCounterexample
    let t := unsafeBeginOpenIgnoringEpoch s 2
    t.phase = .opening ∧
    t.openAttempt = some 2 ∧
    ¬ Step s (.beginOpen 0 2) t := by
  dsimp [closedForCounterexample, unsafeBeginOpenIgnoringEpoch]
  constructor
  · rfl
  constructor
  · rfl
  · intro hStep
    cases hStep <;> simp_all [State.CanBeginOpen]

def closingWithOpenAttempt : State :=
  { phase := .closing
    closeEpoch := 2
    openAttempt := some 2
    cleanupOwner := none
    generation := 1 }

def unsafeFinishOpenWhileClosing (s : State) (attempt : AttemptId) : State :=
  { s with
      phase := .open
      openAttempt := none
      generation := attempt }

/-! Allowing the normal `finishOpen` commit while closing reopens the runtime
    after the final-close linearization point. -/
theorem closing_finish_can_reopen_without_gate :
    let s := closingWithOpenAttempt
    let t := unsafeFinishOpenWhileClosing s 2
    t.phase = .open ∧
    t.generation = 2 ∧
    ¬ Step s (.finishOpen 2) t := by
  dsimp [closingWithOpenAttempt, unsafeFinishOpenWhileClosing]
  constructor
  · rfl
  constructor
  · rfl
  · intro hStep
    cases hStep <;> simp_all

def closedWithActiveCleanupOwner : State :=
  { phase := .closed
    closeEpoch := 3
    openAttempt := none
    cleanupOwner := some .finalClose
    generation := 2 }

def unsafeBeginOpenWithActiveCleanupOwner (s : State) (attempt : AttemptId) : State :=
  { s with phase := .opening, openAttempt := some attempt }

/-! Publishing `Closed` before the cleanup owner leaves the callback stack must
    not make the state reopenable. -/
theorem active_cleanup_owner_can_overlap_reopen :
    let s := closedWithActiveCleanupOwner
    let t := unsafeBeginOpenWithActiveCleanupOwner s 4
    t.phase = .opening ∧
    t.openAttempt = some 4 ∧
    ¬ s.ReturnSafe ∧
    ¬ Step s (.beginOpen 3 4) t := by
  dsimp [closedWithActiveCleanupOwner, unsafeBeginOpenWithActiveCleanupOwner]
  constructor
  · rfl
  constructor
  · rfl
  constructor
  · simp [State.ReturnSafe]
  · intro hStep
    cases hStep <;> simp_all [State.CanBeginOpen]

end XlFnFormal.Lifecycle
