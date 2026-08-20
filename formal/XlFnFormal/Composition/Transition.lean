import XlFnFormal.Composition.Model

set_option autoImplicit false

namespace XlFnFormal.Composition

inductive Event where
  | beginOpen (sampledEpoch : Lifecycle.Epoch) (attempt : Lifecycle.AttemptId)
  | finishOpenRejectedByClose (attempt : Lifecycle.AttemptId)
  | failOpen (attempt : Lifecycle.AttemptId)
  | requestFinalClose
  | acquireFinalCloseOwner
  | acquireOpenRollbackOwner
  | commitOpen (attempt : Lifecycle.AttemptId) (resources : Shutdown.Resources)
  | liftShutdown (event : Shutdown.Event)
  | finishCommittedShutdown
  | publishCommittedClosed
  | retireCommittedShutdown
  | finishUncommittedFinalClose (resources : Shutdown.Resources)
  | finishOpenRollback (resources : Shutdown.Resources)
  | releaseCleanupOwner
  deriving DecidableEq, Repr

inductive Step : State → Event → State → Prop where
  | beginOpen
      {s : State}
      {sampledEpoch : Lifecycle.Epoch}
      {attempt : Lifecycle.AttemptId}
      {t : Lifecycle.State}
      (hNoSession : s.currentShutdown = none)
      (hStep : Lifecycle.Step s.lifecycle
        (.beginOpen sampledEpoch attempt) t) :
      Step s (.beginOpen sampledEpoch attempt)
        { lifecycle := t, currentShutdown := none, logicalQuiescenceCertified := false }

  | finishOpenRejectedByClose
      {s : State}
      {attempt : Lifecycle.AttemptId}
      {t : Lifecycle.State}
      (hNoSession : s.currentShutdown = none)
      (hStep : Lifecycle.Step s.lifecycle
        (.finishOpenRejectedByClose attempt) t) :
      Step s (.finishOpenRejectedByClose attempt)
        { lifecycle := t, currentShutdown := none, logicalQuiescenceCertified := false }

  | failOpen
      {s : State}
      {attempt : Lifecycle.AttemptId}
      {t : Lifecycle.State}
      (hNoSession : s.currentShutdown = none)
      (hStep : Lifecycle.Step s.lifecycle (.failOpen attempt) t) :
      Step s (.failOpen attempt)
        { s with lifecycle := t, currentShutdown := none, logicalQuiescenceCertified := false }

  | requestFinalClose
      {s : State}
      {t : Lifecycle.State}
      (hStep : Lifecycle.Step s.lifecycle .requestFinalClose t) :
      Step s .requestFinalClose { s with lifecycle := t }

  | acquireFinalCloseOwner
      {s : State}
      {t : Lifecycle.State}
      (hStep : Lifecycle.Step s.lifecycle .acquireFinalCloseOwner t) :
      Step s .acquireFinalCloseOwner { s with lifecycle := t }

  | acquireOpenRollbackOwner
      {s : State}
      {t : Lifecycle.State}
      (hNoSession : s.currentShutdown = none)
      (hStep : Lifecycle.Step s.lifecycle .acquireOpenRollbackOwner t) :
      Step s .acquireOpenRollbackOwner
        { s with lifecycle := t, currentShutdown := none }

  | commitOpen
      {s : State}
      {attempt : Lifecycle.AttemptId}
      {resources : Shutdown.Resources}
      (hNoSession : s.currentShutdown = none)
      (hPhase : s.lifecycle.phase = .opening)
      (hAttempt : s.lifecycle.openAttempt = some attempt)
      (hNonzero : attempt ≠ 0) :
      Step s (.commitOpen attempt resources)
        { lifecycle :=
            { s.lifecycle with
              phase := .open
              openAttempt := none
              generation := attempt }
          currentShutdown := some
            { generation := attempt
              state := Shutdown.State.opened resources }
          logicalQuiescenceCertified := false }

  | liftShutdown
      {s : State}
      {session : ShutdownSession}
      {event : Shutdown.Event}
      {shutdown' : Shutdown.State}
      (hSession : s.currentShutdown = some session)
      (hLifecycle : s.lifecycle.phase = .open ∨ s.lifecycle.phase = .closing)
      (hStep : Shutdown.Step session.state event shutdown')
      (hNotFinish : event ≠ .finishClose)
      (hOpenTarget : s.lifecycle.phase = .open → shutdown'.phase = .open) :
      Step s (.liftShutdown event)
        { s with currentShutdown := some { session with state := shutdown' } }

  | finishCommittedShutdown
      {s : State}
      {session : ShutdownSession}
      (hSession : s.currentShutdown = some session)
      (certificate :
        Lifecycle.CommittedCloseCertificate
          s.lifecycle session.generation session.state) :
      Step s .finishCommittedShutdown
        { s with currentShutdown := some (ShutdownSession.closed session) }

  | publishCommittedClosed
      {s : State}
      {session : ShutdownSession}
      (hSession : s.currentShutdown = some session)
      (hPhase : s.lifecycle.phase = .closing)
      (hNoAttempt : s.lifecycle.openAttempt = none)
      (hOwner : s.lifecycle.cleanupOwner = some .finalClose)
      (hShutdownClosed : session.state.phase = .closed) :
      Step s .publishCommittedClosed
        { s with
            lifecycle := { s.lifecycle with phase := .closed }
            logicalQuiescenceCertified := true }

  | retireCommittedShutdown
      {s : State}
      {session : ShutdownSession}
      (hSession : s.currentShutdown = some session)
      (hPhase : s.lifecycle.phase = .closed)
      (hOwner : s.lifecycle.cleanupOwner = some .finalClose)
      (hShutdownClosed : session.state.phase = .closed)
      (hGeneration : session.generation = s.lifecycle.generation) :
      Step s .retireCommittedShutdown { s with currentShutdown := none }

  | finishUncommittedFinalClose
      {s : State}
      {resources : Shutdown.Resources}
      (hNoSession : s.currentShutdown = none)
      (certificate :
        Lifecycle.UncommittedCloseCertificate s.lifecycle resources) :
      Step s (.finishUncommittedFinalClose resources)
        { s with
            lifecycle := { s.lifecycle with phase := .closed }
            currentShutdown := none
            logicalQuiescenceCertified := true }

  | finishOpenRollback
      {s : State}
      {resources : Shutdown.Resources}
      (hNoSession : s.currentShutdown = none)
      (certificate :
        Lifecycle.OpenRollbackCertificate s.lifecycle resources) :
      Step s (.finishOpenRollback resources)
        { s with
            lifecycle := { s.lifecycle with phase := .closed }
            currentShutdown := none
            logicalQuiescenceCertified := true }

  | releaseCleanupOwner
      {s : State}
      {t : Lifecycle.State}
      (hAllowed : s.currentShutdown = none ∨
        s.lifecycle.phase = .closing)
      (hStep : Lifecycle.Step s.lifecycle .releaseCleanupOwner t) :
      Step s .releaseCleanupOwner { s with lifecycle := t }

end XlFnFormal.Composition
