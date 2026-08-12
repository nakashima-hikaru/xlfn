import XlFnFormal.Composition.Trace

set_option autoImplicit false

namespace XlFnFormal.Composition

theorem Step.valid_preserved
    {s t : State} {event : Event}
    (hValid : s.Valid)
    (hStep : Step s event t) :
    t.Valid := by
  cases hStep with
  | beginOpen hNoSession hLifecycle =>
      have hLifecycleValid := Lifecycle.Step.valid_preserved
        hValid.1 hLifecycle
      cases hLifecycle
      exact ⟨hLifecycleValid, by simp [State.SessionConsistent], by
        simp [State.CurrentShutdownCertified]⟩
  | finishOpenRejectedByClose hNoSession hLifecycle =>
      have hLifecycleValid := Lifecycle.Step.valid_preserved
        hValid.1 hLifecycle
      cases hLifecycle with
      | finishOpenRejectedByClose hPhase hAttempt =>
          exact ⟨hLifecycleValid, by
            simp [State.SessionConsistent, hPhase], by
            simp [State.CurrentShutdownCertified]⟩
  | failOpen hNoSession hLifecycle =>
      have hLifecycleValid := Lifecycle.Step.valid_preserved
        hValid.1 hLifecycle
      cases hLifecycle with
      | failOpen hPhase hAttempt =>
          exact ⟨hLifecycleValid, by
            simp [State.SessionConsistent], by
            simp [State.CurrentShutdownCertified]⟩
      | failOpenWhileClosing hPhase hAttempt =>
          exact ⟨hLifecycleValid, by
            simp [State.SessionConsistent, hPhase], by
            simp [State.CurrentShutdownCertified]⟩
  | requestFinalClose hLifecycle =>
      have hLifecycleValid := Lifecycle.Step.valid_preserved
        hValid.1 hLifecycle
      have hConsistent := hValid.2.1
      have hLifecycleWF := hValid.1.1
      cases hLifecycle
      exact ⟨hLifecycleValid, by
        cases hPhase : s.lifecycle.phase <;>
          cases hSession : s.currentShutdown <;>
          simp_all [State.SessionConsistent, Lifecycle.phaseAfterFinalClose,
            State.ShutdownSessionPublished, Lifecycle.State.Valid,
            Lifecycle.State.WellFormed, Lifecycle.State.PhaseConsistent,
            Lifecycle.State.AttemptOwnerDisjoint, Lifecycle.State.OwnerConsistent], by
        simpa [State.CurrentShutdownCertified] using hValid.2.2⟩
  | acquireFinalCloseOwner hLifecycle =>
      have hLifecycleValid := Lifecycle.Step.valid_preserved
        hValid.1 hLifecycle
      have hConsistent := hValid.2.1
      cases hLifecycle
      exact ⟨hLifecycleValid, by
        cases hSession : s.currentShutdown <;>
          simp_all [State.SessionConsistent, State.ShutdownSessionPublished], by
        simpa [State.CurrentShutdownCertified] using hValid.2.2⟩
  | acquireOpenRollbackOwner hNoSession hLifecycle =>
      have hLifecycleValid := Lifecycle.Step.valid_preserved
        hValid.1 hLifecycle
      cases hLifecycle
      exact ⟨hLifecycleValid, by
        cases hPhase : s.lifecycle.phase <;>
          simp_all [State.SessionConsistent], by
        simp [State.CurrentShutdownCertified]⟩
  | @commitOpen attempt resources hNoSession hPhase hAttempt hNonzero =>
      have hLifecycleStep : Lifecycle.Step s.lifecycle (.finishOpen attempt)
          { s.lifecycle with
            phase := .open
            openAttempt := none
            generation := attempt } :=
        Lifecycle.Step.finishOpen hPhase hAttempt
      have hLifecycleValid := Lifecycle.Step.valid_preserved
        hValid.1 hLifecycleStep
      have hCertified : (Shutdown.State.opened resources).Certified :=
        Shutdown.State.certified_of_open rfl
      exact ⟨hLifecycleValid, by
        simp [State.SessionConsistent, Shutdown.State.opened], by
        simpa [State.CurrentShutdownCertified] using hCertified⟩
  | @liftShutdown session event shutdown' hSession hLifecycle hShutdown
      hNotFinish hOpenTarget =>
      have hSourceCertified : session.state.Certified := by
        simpa [State.CurrentShutdownCertified, hSession] using hValid.2.2
      have hTargetCertified : shutdown'.Certified :=
        Shutdown.Step.certified_preserved hSourceCertified hShutdown
      exact ⟨hValid.1, by
        cases hLifecycle with
        | inl hOpen =>
            have h := hValid.2.1
            simp [State.SessionConsistent, hOpen, hSession] at h ⊢
            exact ⟨h.1, hOpenTarget hOpen⟩
        | inr hClosing =>
            have h := hValid.2.1
            simp [State.SessionConsistent, hClosing, hSession] at h ⊢
            exact ⟨h.1, h.2.1, by
              cases shutdown'.phase <;>
                simp [State.ShutdownSessionPublished]⟩, by
        simpa [State.CurrentShutdownCertified] using hTargetCertified⟩
  | @finishCommittedShutdown session shutdown' hSession hPhase hNoAttempt hShutdown =>
      have hSourceCertified : session.state.Certified := by
        simpa [State.CurrentShutdownCertified, hSession] using hValid.2.2
      have hTargetCertified : shutdown'.Certified :=
        Shutdown.Step.certified_preserved hSourceCertified hShutdown
      exact ⟨hValid.1, by
        have h := hValid.2.1
        simp [State.SessionConsistent, hPhase, hSession] at h ⊢
        exact ⟨h.1, h.2.1, by
          cases shutdown'.phase <;>
            simp [State.ShutdownSessionPublished]⟩, by
        simpa [State.CurrentShutdownCertified] using hTargetCertified⟩
  | @publishCommittedClosed session hSession hPhase hNoAttempt hOwner
      hShutdownClosed =>
      have hLifecycleStep : Lifecycle.Step s.lifecycle .finishFinalClose
          { s.lifecycle with phase := .closed } :=
        Lifecycle.Step.finishFinalClose hPhase hNoAttempt hOwner
      have hLifecycleValid := Lifecycle.Step.valid_preserved
        hValid.1 hLifecycleStep
      have h := hValid.2.1
      simp [State.SessionConsistent, hPhase, hSession] at h ⊢
      exact ⟨hLifecycleValid, ⟨h.2.1, hShutdownClosed, hOwner⟩, by
        simpa [State.CurrentShutdownCertified, hSession] using hValid.2.2⟩
  | @retireCommittedShutdown session hSession hPhase hOwner hShutdownClosed
      hGeneration =>
      exact ⟨hValid.1, by simp [State.SessionConsistent, hPhase], by
        simp [State.CurrentShutdownCertified]⟩
  | finishUncommittedFinalClose hNoSession hPhase hNoAttempt hOwner hQuiescent =>
      have hLifecycleStep : Lifecycle.Step s.lifecycle .finishFinalClose
          { s.lifecycle with phase := .closed } :=
        Lifecycle.Step.finishFinalClose hPhase hNoAttempt hOwner
      have hLifecycleValid := Lifecycle.Step.valid_preserved
        hValid.1 hLifecycleStep
      exact ⟨hLifecycleValid, by simp [State.SessionConsistent], by
        simp [State.CurrentShutdownCertified]⟩
  | finishOpenRollback hNoSession hPhase hNoAttempt hOwner hQuiescent =>
      have hLifecycleStep : Lifecycle.Step s.lifecycle .finishOpenRollback
          { s.lifecycle with phase := .closed } :=
        Lifecycle.Step.finishOpenRollback hPhase hNoAttempt hOwner
      have hLifecycleValid := Lifecycle.Step.valid_preserved
        hValid.1 hLifecycleStep
      exact ⟨hLifecycleValid, by simp [State.SessionConsistent], by
        simp [State.CurrentShutdownCertified]⟩
  | releaseCleanupOwner hNoSession hLifecycle =>
      have hLifecycleValid := Lifecycle.Step.valid_preserved
        hValid.1 hLifecycle
      cases hLifecycle
      exact ⟨hLifecycleValid, by
        cases hPhase : s.lifecycle.phase <;>
          simp_all [State.SessionConsistent], by
        simp [State.CurrentShutdownCertified]⟩

theorem Reachable.valid
    {initial current : State}
    (hInitial : initial.Valid)
    (hReachable : Reachable initial current) :
    current.Valid := by
  induction hReachable with
  | initial =>
      exact hInitial
  | step _ hStep ih =>
      exact Step.valid_preserved ih hStep

theorem Steps.valid
    {initial final : State} {events : List Event}
    (hInitial : initial.Valid)
    (hSteps : Steps initial events final) :
    final.Valid :=
  Reachable.valid hInitial hSteps.reachable

end XlFnFormal.Composition
