import XlFnFormal.Composition.Safety
import XlFnFormal.Lifecycle.Checker
import XlFnFormal.Shutdown.Checker

set_option autoImplicit false

namespace XlFnFormal.Composition

/-! The executable composition checker delegates lifecycle and Shutdown
    substeps to their existing checkers.  Cleanup completion is guarded by
    the corresponding Lifecycle certificate predicate, so the checker and the
    relational model share the same proof obligation. -/

private instance committedClosePrerequisitesDecidable
    (lifecycle : Lifecycle.State) (generation : Lifecycle.AttemptId)
    (shutdown : Shutdown.State) :
    Decidable
      (Lifecycle.CommittedClosePrerequisites lifecycle generation shutdown) := by
  unfold Lifecycle.CommittedClosePrerequisites Lifecycle.CleanupReady
    Shutdown.Resources.Quiescent Shutdown.Resources.HostDetached
    Shutdown.Resources.CallsDrained Shutdown.Resources.ReturnsDrained
    Shutdown.Resources.AsyncDrained Shutdown.Resources.SubscriptionsDrained
    Shutdown.Resources.RtdDrained Shutdown.Resources.HandlesDrained
    Shutdown.Resources.GenerationReclaimed Shutdown.Resources.DiagnosticsDrained
  infer_instance

private instance uncommittedClosePrerequisitesDecidable
    (lifecycle : Lifecycle.State) (resources : Shutdown.Resources) :
    Decidable
      (Lifecycle.UncommittedClosePrerequisites lifecycle resources) := by
  unfold Lifecycle.UncommittedClosePrerequisites Lifecycle.CleanupReady
    Shutdown.Resources.Quiescent Shutdown.Resources.HostDetached
    Shutdown.Resources.CallsDrained Shutdown.Resources.ReturnsDrained
    Shutdown.Resources.AsyncDrained Shutdown.Resources.SubscriptionsDrained
    Shutdown.Resources.RtdDrained Shutdown.Resources.HandlesDrained
    Shutdown.Resources.GenerationReclaimed Shutdown.Resources.DiagnosticsDrained
  infer_instance

private instance openRollbackPrerequisitesDecidable
    (lifecycle : Lifecycle.State) (resources : Shutdown.Resources) :
    Decidable
      (Lifecycle.OpenRollbackPrerequisites lifecycle resources) := by
  unfold Lifecycle.OpenRollbackPrerequisites Lifecycle.CleanupReady
    Shutdown.Resources.Quiescent Shutdown.Resources.HostDetached
    Shutdown.Resources.CallsDrained Shutdown.Resources.ReturnsDrained
    Shutdown.Resources.AsyncDrained Shutdown.Resources.SubscriptionsDrained
    Shutdown.Resources.RtdDrained Shutdown.Resources.HandlesDrained
    Shutdown.Resources.GenerationReclaimed Shutdown.Resources.DiagnosticsDrained
  infer_instance

def apply? (s : State) (event : Event) : Option State :=
  match event with
  | .beginOpen sampledEpoch attempt =>
      if s.currentShutdown = none then
        match Lifecycle.apply? s.lifecycle (.beginOpen sampledEpoch attempt) with
        | some lifecycle' =>
            some
              { lifecycle := lifecycle'
                currentShutdown := none
                logicalQuiescenceCertified := false }
        | none => none
      else none
  | .finishOpenRejectedByClose attempt =>
      if s.currentShutdown = none then
        match Lifecycle.apply? s.lifecycle
            (.finishOpenRejectedByClose attempt) with
        | some lifecycle' =>
            some
              { lifecycle := lifecycle'
                currentShutdown := none
                logicalQuiescenceCertified := false }
        | none => none
      else none
  | .failOpen attempt =>
      if s.currentShutdown = none then
        match Lifecycle.apply? s.lifecycle (.failOpen attempt) with
        | some lifecycle' =>
            some
              { lifecycle := lifecycle'
                currentShutdown := none
                logicalQuiescenceCertified := false }
        | none => none
      else none
  | .requestFinalClose =>
      match Lifecycle.apply? s.lifecycle .requestFinalClose with
      | some lifecycle' => some { s with lifecycle := lifecycle' }
      | none => none
  | .acquireFinalCloseOwner =>
      match Lifecycle.apply? s.lifecycle .acquireFinalCloseOwner with
      | some lifecycle' => some { s with lifecycle := lifecycle' }
      | none => none
  | .acquireOpenRollbackOwner =>
      if s.currentShutdown = none then
        match Lifecycle.apply? s.lifecycle .acquireOpenRollbackOwner with
        | some lifecycle' =>
            some { s with lifecycle := lifecycle', currentShutdown := none }
        | none => none
      else none
  | .commitOpen attempt resources =>
      if s.currentShutdown = none ∧ attempt ≠ 0 then
        match Lifecycle.apply? s.lifecycle (.finishOpen attempt) with
        | some lifecycle' =>
            some
              { lifecycle := lifecycle'
                currentShutdown := some
                  { generation := attempt
                    state := Shutdown.State.opened resources }
                logicalQuiescenceCertified := false }
        | none => none
      else none
  | .liftShutdown shutdownEvent =>
      match s.currentShutdown with
      | none => none
      | some session =>
          if (s.lifecycle.phase = .open ∨ s.lifecycle.phase = .closing) ∧
              shutdownEvent ≠ .finishClose then
            match Shutdown.apply? session.state shutdownEvent with
            | some shutdown' =>
                if s.lifecycle.phase = .open ∧ shutdown'.phase ≠ .open then
                  none
                else
                  some (State.withShutdown s
                    (some (ShutdownSession.withState session shutdown')))
            | none => none
          else none
  | .finishCommittedShutdown =>
      match s.currentShutdown with
      | none => none
      | some session =>
          if Lifecycle.CommittedClosePrerequisites
              s.lifecycle session.generation session.state then
            some { s with currentShutdown := some (ShutdownSession.closed session) }
          else none
  | .publishCommittedClosed =>
      match s.currentShutdown with
      | none => none
      | some session =>
          if s.lifecycle.phase = .closing ∧
              s.lifecycle.openAttempt = none ∧
              s.lifecycle.cleanupOwner = some .finalClose ∧
              session.state.phase = .closed then
            some { s with
              lifecycle := { s.lifecycle with phase := .closed }
              logicalQuiescenceCertified := true }
          else none
  | .retireCommittedShutdown =>
      match s.currentShutdown with
      | none => none
      | some session =>
          if s.lifecycle.phase = .closed ∧
              s.lifecycle.cleanupOwner = some .finalClose ∧
              session.state.phase = .closed ∧
              session.generation = s.lifecycle.generation then
            some { s with currentShutdown := none }
          else none
  | .finishUncommittedFinalClose resources =>
      if s.currentShutdown = none ∧
          Lifecycle.UncommittedClosePrerequisites s.lifecycle resources then
        some { s with
          lifecycle := { s.lifecycle with phase := .closed }
          currentShutdown := none
          logicalQuiescenceCertified := true }
      else none
  | .finishOpenRollback resources =>
      if s.currentShutdown = none ∧
          Lifecycle.OpenRollbackPrerequisites s.lifecycle resources then
        some { s with
          lifecycle := { s.lifecycle with phase := .closed }
          currentShutdown := none
          logicalQuiescenceCertified := true }
      else none
  | .releaseCleanupOwner =>
      if s.currentShutdown = none ∨ s.lifecycle.phase = .closing then
        match Lifecycle.apply? s.lifecycle .releaseCleanupOwner with
        | some lifecycle' => some { s with lifecycle := lifecycle' }
        | none => none
      else none

theorem apply?_sound
    {s t : State} {event : Event}
    (h : apply? s event = some t) :
    Step s event t := by
  cases event with
  | beginOpen sampledEpoch attempt =>
      by_cases hNoSession : s.currentShutdown = none
      · simp only [apply?, hNoSession] at h
        cases hLifecycle : Lifecycle.apply? s.lifecycle
            (.beginOpen sampledEpoch attempt) with
        | none =>
            simp [hLifecycle] at h
        | some lifecycle' =>
            rw [hLifecycle] at h
            cases h
            exact Step.beginOpen hNoSession
              (Lifecycle.apply?_sound hLifecycle)
      · simp [apply?, hNoSession] at h
  | finishOpenRejectedByClose attempt =>
      by_cases hNoSession : s.currentShutdown = none
      · simp only [apply?, hNoSession] at h
        cases hLifecycle : Lifecycle.apply? s.lifecycle
            (.finishOpenRejectedByClose attempt) with
        | none =>
            simp [hLifecycle] at h
        | some lifecycle' =>
            rw [hLifecycle] at h
            cases h
            exact Step.finishOpenRejectedByClose hNoSession
              (Lifecycle.apply?_sound hLifecycle)
      · simp [apply?, hNoSession] at h
  | failOpen attempt =>
      by_cases hNoSession : s.currentShutdown = none
      · simp only [apply?, hNoSession] at h
        cases hLifecycle : Lifecycle.apply? s.lifecycle (.failOpen attempt) with
        | none =>
            simp [hLifecycle] at h
        | some lifecycle' =>
            simp [hLifecycle] at h
            cases h
            exact Step.failOpen hNoSession (Lifecycle.apply?_sound hLifecycle)
      · simp [apply?, hNoSession] at h
  | requestFinalClose =>
      simp only [apply?] at h
      cases hLifecycle : Lifecycle.apply? s.lifecycle .requestFinalClose with
      | none =>
          simp [hLifecycle] at h
      | some lifecycle' =>
          simp [hLifecycle] at h
          cases h
          exact Step.requestFinalClose (Lifecycle.apply?_sound hLifecycle)
  | acquireFinalCloseOwner =>
      simp only [apply?] at h
      cases hLifecycle : Lifecycle.apply? s.lifecycle .acquireFinalCloseOwner with
      | none =>
          simp [hLifecycle] at h
      | some lifecycle' =>
          simp [hLifecycle] at h
          cases h
          exact Step.acquireFinalCloseOwner (Lifecycle.apply?_sound hLifecycle)
  | acquireOpenRollbackOwner =>
      by_cases hNoSession : s.currentShutdown = none
      · simp only [apply?, hNoSession] at h
        cases hLifecycle : Lifecycle.apply? s.lifecycle
            .acquireOpenRollbackOwner with
        | none =>
            simp [hLifecycle] at h
        | some lifecycle' =>
            simp [hLifecycle] at h
            cases h
            exact Step.acquireOpenRollbackOwner hNoSession
              (Lifecycle.apply?_sound hLifecycle)
      · simp [apply?, hNoSession] at h
  | commitOpen attempt resources =>
      by_cases hPre : s.currentShutdown = none ∧ attempt ≠ 0
      · simp only [apply?] at h
        rw [if_pos hPre] at h
        cases hLifecycle : Lifecycle.apply? s.lifecycle (.finishOpen attempt) with
        | none =>
            simp only [hLifecycle] at h
            cases h
        | some lifecycle' =>
            simp only [hLifecycle] at h
            cases h
            have hLifecycleStep := Lifecycle.apply?_sound hLifecycle
            cases hLifecycleStep with
            | finishOpen hPhase hAttempt =>
                exact Step.commitOpen (resources := resources)
                  hPre.1 hPhase hAttempt hPre.2
      · simp only [apply?] at h
        rw [if_neg hPre] at h
        cases h
  | liftShutdown shutdownEvent =>
      cases hSession : s.currentShutdown with
      | none =>
          simp [apply?, hSession] at h
      | some session =>
          by_cases hGate :
              (s.lifecycle.phase = .open ∨ s.lifecycle.phase = .closing) ∧
                shutdownEvent ≠ .finishClose
          · simp only [apply?, hSession] at h
            rw [if_pos hGate] at h
            cases hShutdown : Shutdown.apply? session.state shutdownEvent with
            | none =>
                rw [hShutdown] at h
                cases h
            | some shutdown' =>
                by_cases hReject :
                    s.lifecycle.phase = .open ∧ shutdown'.phase ≠ .open
                · simp only [hShutdown] at h
                  rw [if_pos hReject] at h
                  cases h
                · simp only [hShutdown] at h
                  rw [if_neg hReject] at h
                  cases h
                  have hOpenTarget :
                      s.lifecycle.phase = .open → shutdown'.phase = .open := by
                    intro hOpen
                    by_cases hNotOpen : shutdown'.phase ≠ .open
                    · exact False.elim (hReject ⟨hOpen, hNotOpen⟩)
                    · exact Classical.byContradiction (fun hNotOpen' =>
                        hNotOpen hNotOpen')
                  exact Step.liftShutdown hSession hGate.1
                    (Shutdown.apply?_sound hShutdown) hGate.2 hOpenTarget
          · simp only [apply?, hSession] at h
            rw [if_neg hGate] at h
            cases h
  | finishCommittedShutdown =>
      cases hSession : s.currentShutdown with
      | none =>
          simp [apply?, hSession] at h
      | some session =>
          by_cases hCertificate :
              Lifecycle.CommittedClosePrerequisites
                s.lifecycle session.generation session.state
          · simp only [apply?, hSession] at h
            rw [if_pos hCertificate] at h
            cases h
            exact Step.finishCommittedShutdown hSession ⟨hCertificate⟩
          · simp only [apply?, hSession] at h
            rw [if_neg hCertificate] at h
            cases h
  | publishCommittedClosed =>
      cases hSession : s.currentShutdown with
      | none =>
          simp [apply?, hSession] at h
      | some session =>
          by_cases hPre : s.lifecycle.phase = .closing ∧
              s.lifecycle.openAttempt = none ∧
              s.lifecycle.cleanupOwner = some .finalClose ∧
              session.state.phase = .closed
          · simp only [apply?, hSession] at h
            rw [if_pos hPre] at h
            cases h
            simpa [hSession] using
              (Step.publishCommittedClosed (s := s) hSession hPre.1 hPre.2.1
                hPre.2.2.1 hPre.2.2.2)
          · simp only [apply?, hSession] at h
            rw [if_neg hPre] at h
            cases h
  | retireCommittedShutdown =>
      cases hSession : s.currentShutdown with
      | none =>
          simp [apply?, hSession] at h
      | some session =>
          by_cases hPre : s.lifecycle.phase = .closed ∧
              s.lifecycle.cleanupOwner = some .finalClose ∧
              session.state.phase = .closed ∧
              session.generation = s.lifecycle.generation
          · simp only [apply?, hSession] at h
            rw [if_pos hPre] at h
            cases h
            exact Step.retireCommittedShutdown hSession hPre.1 hPre.2.1
              hPre.2.2.1 hPre.2.2.2
          · simp only [apply?, hSession] at h
            rw [if_neg hPre] at h
            cases h
  | finishUncommittedFinalClose resources =>
      by_cases hPre : s.currentShutdown = none ∧
          Lifecycle.UncommittedClosePrerequisites s.lifecycle resources
      · simp only [apply?] at h
        rw [if_pos hPre] at h
        cases h
        exact Step.finishUncommittedFinalClose hPre.1 ⟨hPre.2⟩
      · simp only [apply?] at h
        rw [if_neg hPre] at h
        cases h
  | finishOpenRollback resources =>
      by_cases hPre : s.currentShutdown = none ∧
          Lifecycle.OpenRollbackPrerequisites s.lifecycle resources
      · simp only [apply?] at h
        rw [if_pos hPre] at h
        cases h
        exact Step.finishOpenRollback hPre.1 ⟨hPre.2⟩
      · simp only [apply?] at h
        rw [if_neg hPre] at h
        cases h
  | releaseCleanupOwner =>
      by_cases hAllowed : s.currentShutdown = none ∨
          s.lifecycle.phase = .closing
      · simp only [apply?, hAllowed] at h
        cases hLifecycle : Lifecycle.apply? s.lifecycle .releaseCleanupOwner with
        | none =>
            simp [hLifecycle] at h
        | some lifecycle' =>
            simp [hLifecycle] at h
            cases h
            exact Step.releaseCleanupOwner hAllowed
              (Lifecycle.apply?_sound hLifecycle)
      · simp [apply?, hAllowed] at h

theorem apply?_complete
    {s t : State} {event : Event}
    (h : Step s event t) :
    apply? s event = some t := by
  cases h with
  | beginOpen hNoSession hStep =>
      simp [apply?, hNoSession, Lifecycle.apply?_complete hStep]
  | finishOpenRejectedByClose hNoSession hStep =>
      simp [apply?, hNoSession, Lifecycle.apply?_complete hStep]
  | failOpen hNoSession hStep =>
      simp [apply?, hNoSession, Lifecycle.apply?_complete hStep]
  | requestFinalClose hStep =>
      simp [apply?, Lifecycle.apply?_complete hStep]
  | acquireFinalCloseOwner hStep =>
      simp [apply?, Lifecycle.apply?_complete hStep]
  | acquireOpenRollbackOwner hNoSession hStep =>
      simp [apply?, hNoSession, Lifecycle.apply?_complete hStep]
  | @commitOpen attempt resources hNoSession hPhase hAttempt hNonzero =>
      have hLifecycleStep : Lifecycle.Step s.lifecycle (.finishOpen attempt)
          { s.lifecycle with
            phase := .open
            openAttempt := none
            generation := attempt } :=
        Lifecycle.Step.finishOpen hPhase hAttempt
      simp [apply?, hNoSession, hNonzero,
        Lifecycle.apply?_complete hLifecycleStep]
  | @liftShutdown session event shutdown' hSession hLifecycle hStep
      hNotFinish hOpenTarget =>
      have hReject : ¬(s.lifecycle.phase = .open ∧ shutdown'.phase ≠ .open) := by
        intro hReject
        exact hReject.2 (hOpenTarget hReject.1)
      simp [apply?, hSession, hLifecycle, hNotFinish,
        Shutdown.apply?_complete hStep, hReject,
        State.withShutdown, ShutdownSession.withState]
  | @finishCommittedShutdown session hSession hCertificate =>
      simp [apply?, hSession, hCertificate.prerequisites,
        ShutdownSession.closed]
  | @publishCommittedClosed session hSession hPhase hNoAttempt hOwner
      hShutdownClosed =>
      simp [apply?, hSession, hPhase, hNoAttempt, hOwner, hShutdownClosed]
  | @retireCommittedShutdown session hSession hPhase hOwner hShutdownClosed
      hGeneration =>
      simp [apply?, hSession, hPhase, hOwner, hShutdownClosed, hGeneration]
  | @finishUncommittedFinalClose resources hNoSession hCertificate =>
      simp [apply?, hNoSession, hCertificate.prerequisites]
  | @finishOpenRollback resources hNoSession hCertificate =>
      simp [apply?, hNoSession, hCertificate.prerequisites]
  | releaseCleanupOwner hAllowed hStep =>
      simp [apply?, hAllowed, Lifecycle.apply?_complete hStep]

end XlFnFormal.Composition
