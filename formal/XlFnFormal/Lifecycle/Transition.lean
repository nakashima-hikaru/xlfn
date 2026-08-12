import XlFnFormal.Lifecycle.Model

set_option autoImplicit false

namespace XlFnFormal.Lifecycle

inductive Event where
  | beginOpen (sampledEpoch : Epoch) (attempt : AttemptId)
  | finishOpen (attempt : AttemptId)
  | finishOpenRejectedByClose (attempt : AttemptId)
  | failOpen (attempt : AttemptId)
  | requestFinalClose
  | acquireFinalCloseOwner
  | acquireOpenRollbackOwner
  | finishFinalClose
  | finishOpenRollback
  | releaseCleanupOwner
  deriving DecidableEq, Repr

def phaseAfterFinalClose : Phase → Phase
  | .closed => .closed
  | .opening => .closing
  | .open => .closing
  | .closing => .closing
  | .openRollbackPending => .closing

inductive Step : State → Event → State → Prop where
  | beginOpen
      {s : State}
      {sampledEpoch : Epoch}
      {attempt : AttemptId}
      (hCanOpen : s.CanBeginOpen sampledEpoch)
      (hNonzero : attempt ≠ 0) :
      Step s (.beginOpen sampledEpoch attempt)
        { s with
            phase := .opening
            openAttempt := some attempt }

  | finishOpen
      {s : State}
      {attempt : AttemptId}
      (hPhase : s.phase = .opening)
      (hAttempt : s.openAttempt = some attempt) :
      Step s (.finishOpen attempt)
        { s with
            phase := .open
            openAttempt := none
            generation := attempt }

  | finishOpenRejectedByClose
      {s : State}
      {attempt : AttemptId}
      (hPhase : s.phase = .closing)
      (hAttempt : s.openAttempt = some attempt) :
      Step s (.finishOpenRejectedByClose attempt)
        { s with openAttempt := none }

  | failOpen
      {s : State}
      {attempt : AttemptId}
      (hPhase : s.phase = .opening)
      (hAttempt : s.openAttempt = some attempt) :
      Step s (.failOpen attempt)
        { s with
            phase := .openRollbackPending
            openAttempt := none }

  | failOpenWhileClosing
      {s : State}
      {attempt : AttemptId}
      (hPhase : s.phase = .closing)
      (hAttempt : s.openAttempt = some attempt) :
      Step s (.failOpen attempt) { s with openAttempt := none }

  | requestFinalClose
      {s : State} :
      Step s .requestFinalClose
        { s with
            phase := phaseAfterFinalClose s.phase
            closeEpoch := s.closeEpoch + 1 }

  | acquireFinalCloseOwner
      {s : State}
      (hPhase : s.phase = .closing)
      (hNoAttempt : s.openAttempt = none)
      (hNoOwner : s.cleanupOwner = none) :
      Step s .acquireFinalCloseOwner
        { s with cleanupOwner := some .finalClose }

  | acquireOpenRollbackOwner
      {s : State}
      (hPhase : s.phase = .openRollbackPending)
      (hNoAttempt : s.openAttempt = none)
      (hNoOwner : s.cleanupOwner = none) :
      Step s .acquireOpenRollbackOwner
        { s with cleanupOwner := some .openRollback }

  | finishFinalClose
      {s : State}
      (hPhase : s.phase = .closing)
      (hNoAttempt : s.openAttempt = none)
      (hOwner : s.cleanupOwner = some .finalClose) :
      Step s .finishFinalClose { s with phase := .closed }

  | finishOpenRollback
      {s : State}
      (hPhase : s.phase = .openRollbackPending ∨ s.phase = .closing)
      (hNoAttempt : s.openAttempt = none)
      (hOwner : s.cleanupOwner = some .openRollback) :
      Step s .finishOpenRollback { s with phase := .closed }

  | releaseCleanupOwner
      {s : State}
      (hPhase : s.phase = .openRollbackPending ∨
        s.phase = .closing ∨ s.phase = .closed)
      (hNoAttempt : s.openAttempt = none)
      (hOwner : s.cleanupOwner.isSome) :
      Step s .releaseCleanupOwner { s with cleanupOwner := none }

end XlFnFormal.Lifecycle
