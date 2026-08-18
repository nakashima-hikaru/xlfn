import XlFnFormal.Shutdown.Transition

set_option autoImplicit false

namespace XlFnFormal.Shutdown

theorem Step.callsDrained_postcondition
    {s t : State}
    (hStep : Step s .callsDrained t) :
    t.phase = .closing .drainReturns ∧ t.resources.CallsDrained := by
  cases hStep with
  | callsDrained _ hDrained => exact ⟨rfl, hDrained⟩

theorem Step.returnsDrained_postcondition
    {s t : State}
    (hStep : Step s .returnsDrained t) :
    t.phase = .closing .drainAsync ∧ t.resources.ReturnsDrained := by
  cases hStep with
  | returnsDrained _ hDrained => exact ⟨rfl, hDrained⟩

theorem Step.asyncDrained_postcondition
    {s t : State}
    (hStep : Step s .asyncDrained t) :
    t.phase = .closing .stopSubscriptions ∧ t.resources.AsyncDrained := by
  cases hStep with
  | asyncDrained _ hDrained => exact ⟨rfl, hDrained⟩

theorem Step.subscriptionsDrained_postcondition
    {s t : State}
    (hStep : Step s .subscriptionsDrained t) :
    t.phase = .closing .detachHost ∧ t.resources.SubscriptionsDrained := by
  cases hStep with
  | subscriptionsDrained _ hDrained => exact ⟨rfl, hDrained⟩

theorem Step.hostDetached_postcondition
    {s t : State}
    (hStep : Step s .hostDetached t) :
    t.phase = .closing .closeState ∧ t.resources.HostDetached := by
  cases hStep with
  | hostDetached _ hDetached => exact ⟨rfl, hDetached⟩

theorem Step.generationReclaimed_postcondition
    {s t : State}
    (hStep : Step s .generationReclaimed t) :
    t.phase = .closing .drainHandles ∧ t.resources.GenerationReclaimed := by
  cases hStep with
  | generationReclaimed _ hUnique hQuiesced _ =>
      exact ⟨rfl, ⟨hUnique, hQuiesced, rfl⟩⟩

theorem Step.handlesDrained_postcondition
    {s t : State}
    (hStep : Step s .handlesDrained t) :
    t.phase = .closing .stopDiagnostics ∧ t.resources.HandlesDrained := by
  cases hStep with
  | handlesDrained _ hDrained => exact ⟨rfl, hDrained⟩

theorem Step.diagnosticsDrained_postcondition
    {s t : State}
    (hStep : Step s .diagnosticsDrained t) :
    t.phase = .closing .drainRtd ∧ t.resources.DiagnosticsDrained := by
  cases hStep with
  | diagnosticsDrained _ hDrained => exact ⟨rfl, hDrained⟩

theorem Step.rtdDrained_postcondition
    {s t : State}
    (hStep : Step s .rtdDrained t) :
    t.phase = .closing .finalize ∧ t.resources.RtdDrained := by
  cases hStep with
  | rtdDrained _ hDrained => exact ⟨rfl, hDrained⟩

theorem Step.finishClose_postcondition
    {s t : State}
    (hStep : Step s .finishClose t) :
    t.phase = .closed ∧ t.resources.Quiescent := by
  cases hStep with
  | finishClose _ hQuiescent => exact ⟨rfl, hQuiescent⟩

end XlFnFormal.Shutdown
