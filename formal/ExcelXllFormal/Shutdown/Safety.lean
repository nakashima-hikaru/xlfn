import ExcelXllFormal.Shutdown.Invariant
import ExcelXllFormal.Shutdown.Milestones

set_option autoImplicit false

namespace ExcelXllFormal.Shutdown

/-- A certified transition never moves the shutdown phase backwards. -/
theorem Step.phaseRank_mono
    {s t : State} {event : Event}
    (hStep : Step s event t) :
    s.phase.rank ≤ t.phase.rank := by
  cases hStep <;>
    simp_all [Phase.rank, CloseStage.rank]
  exact Phase.rank_le_terminal _

/-- External host work can be admitted only while the runtime is open. -/
theorem Step.externalAdmission_requires_open
    {s t : State} {event : Event}
    (hStep : Step s event t)
    (hAdmission : event.IsExternalAdmission) :
    s.phase = .open := by
  cases hStep <;> simp_all [Event.IsExternalAdmission]

/-- One certified transition cannot reopen a runtime once closing has begun. -/
theorem Step.nonopen_preserved
    {s t : State} {event : Event}
    (hNotOpen : s.phase ≠ .open)
    (hStep : Step s event t) :
    t.phase ≠ .open := by
  intro hOpen
  cases hStep <;>
    simp_all [Phase.IsLive, Phase.AllowsReturnCreation,
      Phase.AllowsReturnFree, Phase.AllowsAsyncCreation,
      Phase.AllowsRtdCreation, Phase.AllowsHandleCreation,
      Phase.AllowsStateResourceCreation]

/-- `closed` is terminal: no certified event can run after successful close. -/
theorem closed_terminal
    {s t : State} {event : Event}
    (hClosed : s.phase = .closed) :
    ¬ Step s event t := by
  intro hStep
  cases hStep <;>
    simp_all [Phase.IsLive, Phase.AllowsReturnCreation,
      Phase.AllowsReturnFree, Phase.AllowsAsyncCreation,
      Phase.AllowsRtdCreation, Phase.AllowsHandleCreation,
      Phase.AllowsStateResourceCreation]

/-- `failStopped` is also terminal.  It represents refusing to unload rather
than pretending shutdown succeeded. -/
theorem failStopped_terminal
    {s t : State} {event : Event} {reason : Failure}
    (hFailed : s.phase = .failStopped reason) :
    ¬ Step s event t := by
  intro hStep
  cases hStep <;>
    simp_all [Phase.IsLive, Phase.AllowsReturnCreation,
      Phase.AllowsReturnFree, Phase.AllowsAsyncCreation,
      Phase.AllowsRtdCreation, Phase.AllowsHandleCreation,
      Phase.AllowsStateResourceCreation]

/-- The only transition into `closed` carries a proof of complete quiescence. -/
theorem Step.closed_target_is_quiescent
    {s t : State} {event : Event}
    (hStep : Step s event t)
    (hClosed : t.phase = .closed) :
    t.resources.Quiescent := by
  cases hStep <;>
    simp_all [Phase.IsLive, Phase.AllowsReturnCreation,
      Phase.AllowsReturnFree, Phase.AllowsAsyncCreation,
      Phase.AllowsRtdCreation, Phase.AllowsHandleCreation,
      Phase.AllowsStateResourceCreation]

/-- Consequently, every reachable successful state is safe to unload. -/
theorem reachable_closed_is_quiescent
    {initial current : State}
    (hInitialOpen : initial.phase = .open)
    (hReachable : Reachable initial current)
    (hClosed : current.phase = .closed) :
    current.resources.Quiescent := by
  have hCertified := Reachable.certified
    (State.certified_of_open hInitialOpen) hReachable
  exact State.quiescent_of_certified_closed hCertified hClosed

/-- Trace-level form of the main shutdown theorem. -/
theorem Steps.successful_shutdown_is_quiescent
    {initial final : State}
    {events : List Event}
    (hSteps : Steps initial events final)
    (hInitialOpen : initial.phase = .open)
    (hClosed : final.phase = .closed) :
    final.resources.Quiescent :=
  reachable_closed_is_quiescent hInitialOpen hSteps.reachable hClosed

/-- A successful close has no escaped state lease. -/
theorem reachable_closed_has_no_state_escape
    {initial current : State}
    (hInitialOpen : initial.phase = .open)
    (hReachable : Reachable initial current)
    (hClosed : current.phase = .closed) :
    current.resources.externalStateLeases = 0 := by
  have hQuiescent := reachable_closed_is_quiescent
    hInitialOpen hReachable hClosed
  rcases Resources.quiescent_stateClosed hQuiescent with
    ⟨hLeases, _, _, _, _⟩
  exact hLeases

/-- A successful close has no worker or accepted worker job. -/
theorem reachable_closed_has_no_worker
    {initial current : State}
    (hInitialOpen : initial.phase = .open)
    (hReachable : Reachable initial current)
    (hClosed : current.phase = .closed) :
    current.resources.workers = 0 ∧ current.resources.workerJobs = 0 := by
  have hQuiescent := reachable_closed_is_quiescent
    hInitialOpen hReachable hClosed
  rcases Resources.quiescent_stateClosed hQuiescent with
    ⟨_, hWorkers, hJobs, _, _⟩
  exact ⟨hWorkers, hJobs⟩

/-- A successful close has no executable module work, no worksheet return
awaiting `xlAutoFree12`, and no subsystem-owned value that can invoke module
code during destruction. -/
theorem reachable_closed_has_no_executable_work
    {initial current : State}
    (hInitialOpen : initial.phase = .open)
    (hReachable : Reachable initial current)
    (hClosed : current.phase = .closed) :
    current.resources.activeCalls = 0 ∧
    current.resources.returnBlocks = 0 ∧
    current.resources.returnFreeOperations = 0 ∧
    current.resources.asyncTasks = 0 ∧
    current.resources.asyncExecutorRunning = false ∧
    current.resources.rtdOperations = 0 ∧
    current.resources.subscriptions = 0 ∧
    current.resources.callbacks = 0 ∧
    current.resources.rtdClassFactories = 0 ∧
    current.resources.rtdServers = 0 ∧
    current.resources.rtdServerLocks = 0 ∧
    current.resources.handleOperations = 0 ∧
    current.resources.handles = 0 := by
  have hQuiescent := reachable_closed_is_quiescent
    hInitialOpen hReachable hClosed
  have hCalls := Resources.quiescent_callsDrained hQuiescent
  have hReturns := Resources.quiescent_returnsDrained hQuiescent
  have hAsync := Resources.quiescent_asyncDrained hQuiescent
  have hRtd := Resources.quiescent_rtdDrained hQuiescent
  have hHandles := Resources.quiescent_handlesDrained hQuiescent
  rcases hReturns with ⟨hBlocks, hFree⟩
  rcases hAsync with ⟨hTasks, hExecutor⟩
  rcases hRtd with
    ⟨hOperations, hSubscriptions, hCallbacks, hFactories, hServers, hLocks⟩
  rcases hHandles with ⟨hHandleOperations, hHandleValues⟩
  exact ⟨hCalls, hBlocks, hFree, hTasks, hExecutor,
    hOperations, hSubscriptions, hCallbacks, hFactories, hServers, hLocks,
    hHandleOperations, hHandleValues⟩

/-- A successful close has detached all host registrations, consumed the
state root, and joined diagnostics. -/
theorem reachable_closed_has_no_host_or_dispatcher
    {initial current : State}
    (hInitialOpen : initial.phase = .open)
    (hReachable : Reachable initial current)
    (hClosed : current.phase = .closed) :
    current.resources.registrations = 0 ∧
    current.resources.eventRegistrations = 0 ∧
    current.resources.stateOwnedByRuntime = false ∧
    current.resources.diagnosticsPending = 0 ∧
    current.resources.diagnosticsRunning = false := by
  have hQuiescent := reachable_closed_is_quiescent
    hInitialOpen hReachable hClosed
  rcases Resources.quiescent_hostDetached hQuiescent with
    ⟨hRegistrations, hEvents⟩
  rcases Resources.quiescent_stateClosed hQuiescent with
    ⟨_, _, _, _, hStateOwned⟩
  rcases Resources.quiescent_diagnosticsDrained hQuiescent with
    ⟨hPending, hRunning⟩
  exact ⟨hRegistrations, hEvents, hStateOwned, hPending, hRunning⟩

/-- A single trace cannot decrease lifecycle progress. -/
theorem Steps.phaseRank_mono
    {s t : State} {events : List Event}
    (hSteps : Steps s events t) :
    s.phase.rank ≤ t.phase.rank := by
  induction hSteps with
  | refl =>
      exact Nat.le_refl _
  | cons hStep _ ih =>
      exact Nat.le_trans hStep.phaseRank_mono ih

/-- Once close has begun, a later certified trace cannot return to `open`. -/
theorem Steps.never_reopens
    {s t : State} {events : List Event}
    (hNotOpen : s.phase ≠ .open)
    (hSteps : Steps s events t) :
    t.phase ≠ .open := by
  intro hOpen
  have hRank := hSteps.phaseRank_mono
  have hSourcePositive : 1 ≤ s.phase.rank := by
    cases hPhase : s.phase with
    | «open» => exact False.elim (hNotOpen hPhase)
    | closing stage =>
        cases stage <;> simp [Phase.rank, CloseStage.rank]
    | closed => simp [Phase.rank]
    | failStopped reason => simp [Phase.rank]
  have hTargetZero : t.phase.rank = 0 := by
    simp [hOpen, Phase.rank]
  omega

/-- No externally admitted operation occurs anywhere in a trace that starts
after the close gate has been linearized. -/
theorem Steps.no_external_admission_after_close
    {s t : State} {events : List Event}
    (hNotOpen : s.phase ≠ .open)
    (hSteps : Steps s events t) :
    ∀ event, event ∈ events → ¬ event.IsExternalAdmission := by
  induction hSteps with
  | refl =>
      intro event hMember
      simp at hMember
  | cons hStep hTail ih =>
      intro event hMember hAdmission
      simp only [List.mem_cons] at hMember
      cases hMember with
      | inl hCurrent =>
          subst event
          exact hNotOpen
            (hStep.externalAdmission_requires_open hAdmission)
      | inr hLater =>
          exact ih (Step.nonopen_preserved hNotOpen hStep) event hLater hAdmission

/-- A fail-stop event can never be followed by successful close. -/
theorem failStop_cannot_reach_closed
    {s failed final : State}
    {reason : Failure} {events : List Event}
    (hFail : Step s (.failStop reason) failed)
    (hTail : Steps failed events final) :
    final.phase ≠ .closed := by
  have hFailedPhase : failed.phase = .failStopped reason := by
    cases hFail <;> rfl
  cases hTail with
  | refl =>
      simp [hFailedPhase]
  | cons hStep _ =>
      exact False.elim (failStopped_terminal hFailedPhase hStep)

/-- In particular, detecting an escaped `Arc<State>` is incompatible with a
successful return. -/
theorem stateEscape_cannot_reach_closed
    {s failed final : State} {events : List Event}
    (hFail : Step s (.failStop .stateEscaped) failed)
    (hTail : Steps failed events final) :
    final.phase ≠ .closed :=
  failStop_cannot_reach_closed hFail hTail

/-- Finalization is available exactly through a quiescent source state. -/
theorem finishClose_source_is_quiescent
    {s t : State}
    (hFinish : Step s .finishClose t) :
    s.phase = .closing .finalize ∧ s.resources.Quiescent := by
  cases hFinish with
  | finishClose hStage hQuiescent =>
      exact ⟨hStage, hQuiescent⟩

/-- No non-quiescent state can take the successful finalization transition. -/
theorem nonquiescent_cannot_finish
    {s t : State}
    (hNotQuiescent : ¬ s.resources.Quiescent) :
    ¬ Step s .finishClose t := by
  intro hFinish
  exact hNotQuiescent (finishClose_source_is_quiescent hFinish).2

/-- The proof obligations exported as a single theorem bundle. -/
structure ShutdownSafety where
  certifiedInvariantPreserved :
    ∀ {s t : State} {event : Event},
      s.Certified → Step s event t → t.Certified
  successfulCloseIsQuiescent :
    ∀ {s t : State} {event : Event},
      Step s event t → t.phase = .closed → t.resources.Quiescent
  closedIsTerminal :
    ∀ {s t : State} {event : Event},
      s.phase = .closed → ¬ Step s event t
  failStoppedIsTerminal :
    ∀ {s t : State} {event : Event} {reason : Failure},
      s.phase = .failStopped reason → ¬ Step s event t
  phaseProgressIsMonotone :
    ∀ {s t : State} {event : Event},
      Step s event t → s.phase.rank ≤ t.phase.rank
  externalAdmissionRequiresOpen :
    ∀ {s t : State} {event : Event},
      Step s event t → event.IsExternalAdmission → s.phase = .open
  nonquiescentCannotFinish :
    ∀ {s t : State},
      (¬ s.resources.Quiescent) → ¬ Step s .finishClose t

/-- Machine-checked safety certificate for the shutdown transition system. -/
theorem shutdownSafety : ShutdownSafety :=
  { certifiedInvariantPreserved := fun hCertified hStep =>
      Step.certified_preserved hCertified hStep
    successfulCloseIsQuiescent := fun hStep hClosed =>
      hStep.closed_target_is_quiescent hClosed
    closedIsTerminal := fun hClosed => closed_terminal hClosed
    failStoppedIsTerminal := fun hFailed => failStopped_terminal hFailed
    phaseProgressIsMonotone := fun hStep => hStep.phaseRank_mono
    externalAdmissionRequiresOpen := fun hStep hAdmission =>
      hStep.externalAdmission_requires_open hAdmission
    nonquiescentCannotFinish := fun hNot =>
      nonquiescent_cannot_finish hNot }

end ExcelXllFormal.Shutdown
