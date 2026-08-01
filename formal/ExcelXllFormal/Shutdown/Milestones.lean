import ExcelXllFormal.Shutdown.Transition

set_option autoImplicit false

namespace ExcelXllFormal.Shutdown

/-- Successful host detachment establishes the exact postcondition consumed by
`Runtime::wait_for_calls`. -/
theorem Step.hostDetached_postcondition
    {s t : State}
    (hStep : Step s .hostDetached t) :
    t.phase = .closing .drainCalls ∧ t.resources.HostDetached := by
  cases hStep with
  | hostDetached _ hDetached =>
      exact ⟨rfl, hDetached⟩

/-- Advancing past synchronous calls certifies that no `CallGuard` remains. -/
theorem Step.callsDrained_postcondition
    {s t : State}
    (hStep : Step s .callsDrained t) :
    t.phase = .closing .drainReturns ∧ t.resources.CallsDrained := by
  cases hStep with
  | callsDrained _ hDrained =>
      exact ⟨rfl, hDrained⟩

/-- Advancing past worksheet returns certifies that Excel owns no DLL return
block and that no `xlAutoFree12` callback is executing. -/
theorem Step.returnsDrained_postcondition
    {s t : State}
    (hStep : Step s .returnsDrained t) :
    t.phase = .closing .drainAsync ∧ t.resources.ReturnsDrained := by
  cases hStep with
  | returnsDrained _ hDrained =>
      exact ⟨rfl, hDrained⟩

/-- Advancing past async certifies both an empty task registry and a joined
executor. -/
theorem Step.asyncDrained_postcondition
    {s t : State}
    (hStep : Step s .asyncDrained t) :
    t.phase = .closing .drainRtd ∧ t.resources.AsyncDrained := by
  cases hStep with
  | asyncDrained _ hDrained =>
      exact ⟨rfl, hDrained⟩

/-- Advancing past RTD certifies every RTD/COM resource class. -/
theorem Step.rtdDrained_postcondition
    {s t : State}
    (hStep : Step s .rtdDrained t) :
    t.phase = .closing .drainHandles ∧ t.resources.RtdDrained := by
  cases hStep with
  | rtdDrained _ hDrained =>
      exact ⟨rfl, hDrained⟩

/-- Advancing past handles certifies both in-flight operations and values. -/
theorem Step.handlesDrained_postcondition
    {s t : State}
    (hStep : Step s .handlesDrained t) :
    t.phase = .closing .closeState ∧ t.resources.HandlesDrained := by
  cases hStep with
  | handlesDrained _ hDrained =>
      exact ⟨rfl, hDrained⟩

/-- A successful state close consumes the runtime root only after every
external lease, worker, worker job, and Add-in resource has disappeared. -/
theorem Step.stateClosed_postcondition
    {s t : State}
    (hStep : Step s .stateClosed t) :
    t.phase = .closing .stopDiagnostics ∧ t.resources.StateClosed := by
  cases hStep with
  | stateClosed _ hLeases hWorkers hJobs hResources hOwned =>
      exact ⟨rfl, ⟨hLeases, hWorkers, hJobs, hResources, rfl⟩⟩

/-- Diagnostics are fully stopped before entering the final stage. -/
theorem Step.diagnosticsDrained_postcondition
    {s t : State}
    (hStep : Step s .diagnosticsDrained t) :
    t.phase = .closing .finalize ∧ t.resources.DiagnosticsDrained := by
  cases hStep with
  | diagnosticsDrained _ hDrained =>
      exact ⟨rfl, hDrained⟩

/-- Finalization changes only the phase; all resource postconditions remain
true in the successful terminal state. -/
theorem Step.finishClose_postcondition
    {s t : State}
    (hStep : Step s .finishClose t) :
    t.phase = .closed ∧ t.resources.Quiescent := by
  cases hStep with
  | finishClose _ hQuiescent =>
      exact ⟨rfl, hQuiescent⟩

end ExcelXllFormal.Shutdown
