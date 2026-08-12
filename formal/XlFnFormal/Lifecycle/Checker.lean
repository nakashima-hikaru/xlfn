import XlFnFormal.Lifecycle.Safety

set_option autoImplicit false

namespace XlFnFormal.Lifecycle

/-! The executable transition function mirrors the small synchronization
    protocol.  It is intentionally independent of the Shutdown resource
    machine; composition is a later refinement batch. -/
def apply? (s : State) (event : Event) : Option State :=
  match event with
  | .beginOpen sampledEpoch attempt =>
      if s.phase = .closed ∧
          s.openAttempt = none ∧
          s.cleanupOwner = none ∧
          sampledEpoch = s.closeEpoch ∧
          attempt ≠ 0 then
        some { s with phase := .opening, openAttempt := some attempt }
      else none
  | .finishOpen attempt =>
      if s.phase = .opening ∧ s.openAttempt = some attempt then
        some { s with phase := .open, openAttempt := none, generation := attempt }
      else none
  | .finishOpenRejectedByClose attempt =>
      if s.phase = .closing ∧ s.openAttempt = some attempt then
        some { s with openAttempt := none }
      else none
  | .failOpen attempt =>
      if s.phase = .opening ∧ s.openAttempt = some attempt then
        some { s with phase := .openRollbackPending, openAttempt := none }
      else if s.phase = .closing ∧ s.openAttempt = some attempt then
        some { s with openAttempt := none }
      else none
  | .requestFinalClose =>
      some { s with
        phase := phaseAfterFinalClose s.phase
        closeEpoch := s.closeEpoch + 1 }
  | .acquireFinalCloseOwner =>
      if s.phase = .closing ∧
          s.openAttempt = none ∧
          s.cleanupOwner = none then
        some { s with cleanupOwner := some .finalClose }
      else none
  | .acquireOpenRollbackOwner =>
      if s.phase = .openRollbackPending ∧
          s.openAttempt = none ∧
          s.cleanupOwner = none then
        some { s with cleanupOwner := some .openRollback }
      else none
  | .finishFinalClose =>
      if s.phase = .closing ∧
          s.openAttempt = none ∧
          s.cleanupOwner = some .finalClose then
        some { s with phase := .closed }
      else none
  | .finishOpenRollback =>
      if (s.phase = .openRollbackPending ∨ s.phase = .closing) ∧
          s.openAttempt = none ∧
          s.cleanupOwner = some .openRollback then
        some { s with phase := .closed }
      else none
  | .releaseCleanupOwner =>
      if (s.phase = .openRollbackPending ∨
          s.phase = .closing ∨ s.phase = .closed) ∧
          s.openAttempt = none ∧
          s.cleanupOwner.isSome then
        some { s with cleanupOwner := none }
      else none

theorem apply?_sound
    {s t : State} {event : Event}
    (h : apply? s event = some t) :
    Step s event t := by
  cases event with
  | beginOpen sampledEpoch attempt =>
      by_cases hPre : s.phase = .closed ∧
          s.openAttempt = none ∧
          s.cleanupOwner = none ∧
          sampledEpoch = s.closeEpoch ∧
          attempt ≠ 0
      · simp only [apply?] at h
        rw [if_pos hPre] at h
        cases h
        exact Step.beginOpen
          ⟨hPre.1, hPre.2.1, hPre.2.2.1, hPre.2.2.2.1⟩
          hPre.2.2.2.2
      · simp only [apply?] at h
        rw [if_neg hPre] at h
        cases h
  | finishOpen attempt =>
      by_cases hPre : s.phase = .opening ∧ s.openAttempt = some attempt
      · simp only [apply?] at h
        rw [if_pos hPre] at h
        cases h
        exact Step.finishOpen hPre.1 hPre.2
      · simp only [apply?] at h
        rw [if_neg hPre] at h
        cases h
  | finishOpenRejectedByClose attempt =>
      by_cases hPre : s.phase = .closing ∧ s.openAttempt = some attempt
      · simp only [apply?] at h
        rw [if_pos hPre] at h
        cases h
        exact Step.finishOpenRejectedByClose hPre.1 hPre.2
      · simp only [apply?] at h
        rw [if_neg hPre] at h
        cases h
  | failOpen attempt =>
      by_cases hOpening : s.phase = .opening ∧ s.openAttempt = some attempt
      · simp only [apply?] at h
        rw [if_pos hOpening] at h
        cases h
        exact Step.failOpen hOpening.1 hOpening.2
      · by_cases hClosing : s.phase = .closing ∧ s.openAttempt = some attempt
        · simp only [apply?] at h
          rw [if_neg hOpening, if_pos hClosing] at h
          cases h
          exact Step.failOpenWhileClosing hClosing.1 hClosing.2
        · simp only [apply?] at h
          rw [if_neg hOpening, if_neg hClosing] at h
          cases h
  | requestFinalClose =>
      simp [apply?] at h
      cases h
      exact Step.requestFinalClose
  | acquireFinalCloseOwner =>
      by_cases hPre : s.phase = .closing ∧
          s.openAttempt = none ∧ s.cleanupOwner = none
      · simp only [apply?] at h
        rw [if_pos hPre] at h
        cases h
        exact Step.acquireFinalCloseOwner hPre.1 hPre.2.1 hPre.2.2
      · simp only [apply?] at h
        rw [if_neg hPre] at h
        cases h
  | acquireOpenRollbackOwner =>
      by_cases hPre : s.phase = .openRollbackPending ∧
          s.openAttempt = none ∧ s.cleanupOwner = none
      · simp only [apply?] at h
        rw [if_pos hPre] at h
        cases h
        exact Step.acquireOpenRollbackOwner hPre.1 hPre.2.1 hPre.2.2
      · simp only [apply?] at h
        rw [if_neg hPre] at h
        cases h
  | finishFinalClose =>
      by_cases hPre : s.phase = .closing ∧
          s.openAttempt = none ∧ s.cleanupOwner = some .finalClose
      · simp only [apply?] at h
        rw [if_pos hPre] at h
        cases h
        exact Step.finishFinalClose hPre.1 hPre.2.1 hPre.2.2
      · simp only [apply?] at h
        rw [if_neg hPre] at h
        cases h
  | finishOpenRollback =>
      by_cases hPre : (s.phase = .openRollbackPending ∨ s.phase = .closing) ∧
          s.openAttempt = none ∧ s.cleanupOwner = some .openRollback
      · simp only [apply?] at h
        rw [if_pos hPre] at h
        cases h
        exact Step.finishOpenRollback hPre.1 hPre.2.1 hPre.2.2
      · simp only [apply?] at h
        rw [if_neg hPre] at h
        cases h
  | releaseCleanupOwner =>
      by_cases hPre : (s.phase = .openRollbackPending ∨
          s.phase = .closing ∨ s.phase = .closed) ∧
          s.openAttempt = none ∧ s.cleanupOwner.isSome
      · simp only [apply?] at h
        rw [if_pos hPre] at h
        cases h
        exact Step.releaseCleanupOwner hPre.1 hPre.2.1 hPre.2.2
      · simp only [apply?] at h
        rw [if_neg hPre] at h
        cases h

theorem apply?_complete
    {s t : State} {event : Event}
    (h : Step s event t) :
    apply? s event = some t := by
  cases h <;>
    simp_all [apply?, State.CanBeginOpen, phaseAfterFinalClose]

end XlFnFormal.Lifecycle
