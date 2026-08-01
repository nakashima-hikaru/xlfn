import ExcelXllFormal.Shutdown.Trace

set_option autoImplicit false

namespace ExcelXllFormal.Shutdown

namespace State

/--
The cumulative shutdown certificate associated with each lifecycle phase.

Each milestone adds one more permanently drained subsystem.  This predicate is
stronger than merely recording the current `CloseStage`: it records all
postconditions established by earlier stages and is preserved by every
certified transition.
-/
def Certified (s : State) : Prop :=
  match s.phase with
  | .open => True
  | .closing .detachHost => True
  | .closing .drainCalls =>
      s.resources.HostDetached
  | .closing .drainReturns =>
      s.resources.HostDetached ∧
      s.resources.CallsDrained
  | .closing .drainAsync =>
      s.resources.HostDetached ∧
      s.resources.CallsDrained ∧
      s.resources.ReturnsDrained
  | .closing .drainRtd =>
      s.resources.HostDetached ∧
      s.resources.CallsDrained ∧
      s.resources.ReturnsDrained ∧
      s.resources.AsyncDrained
  | .closing .drainHandles =>
      s.resources.HostDetached ∧
      s.resources.CallsDrained ∧
      s.resources.ReturnsDrained ∧
      s.resources.AsyncDrained ∧
      s.resources.RtdDrained
  | .closing .closeState =>
      s.resources.HostDetached ∧
      s.resources.CallsDrained ∧
      s.resources.ReturnsDrained ∧
      s.resources.AsyncDrained ∧
      s.resources.RtdDrained ∧
      s.resources.HandlesDrained
  | .closing .stopDiagnostics =>
      s.resources.HostDetached ∧
      s.resources.CallsDrained ∧
      s.resources.ReturnsDrained ∧
      s.resources.AsyncDrained ∧
      s.resources.RtdDrained ∧
      s.resources.HandlesDrained ∧
      s.resources.StateClosed
  | .closing .finalize =>
      s.resources.Quiescent
  | .closed =>
      s.resources.Quiescent
  | .failStopped _ => True

/-- Every open state satisfies the initial, vacuous shutdown certificate. -/
theorem certified_of_open {s : State} (hOpen : s.phase = .open) : s.Certified := by
  simp [Certified, hOpen]

/-- A certified successful state is exactly quiescent. -/
theorem quiescent_of_certified_closed
    {s : State}
    (hCertified : s.Certified)
    (hClosed : s.phase = .closed) :
    s.resources.Quiescent := by
  simpa [Certified, hClosed] using hCertified

end State

section CertifiedPreservation

set_option maxHeartbeats 1000000
set_option linter.unusedSimpArgs false

/--
Every legal one-step transition preserves the cumulative shutdown certificate.

This theorem is the central inductive argument of the model.  The creation
gates in `Phase` ensure that, after a subsystem has been certified as drained,
no later transition can recreate one of its resources.  Completion transitions
can only decrease counters, while each milestone constructor supplies the next
postcondition.
-/
theorem Step.certified_preserved
    {s t : State} {event : Event}
    (hCertified : s.Certified)
    (hStep : Step s event t) :
    t.Certified := by
  cases hPhase : s.phase with
  | «open» =>
      cases hStep <;>
        simp_all [State.Certified,
        Resources.HostDetached, Resources.CallsDrained,
        Resources.ReturnsDrained, Resources.AsyncDrained,
        Resources.RtdDrained, Resources.HandlesDrained,
        Resources.StateClosed, Resources.DiagnosticsDrained,
        Resources.Quiescent,
        Phase.IsLive, Phase.AllowsReturnCreation,
        Phase.AllowsReturnFree, Phase.AllowsAsyncCreation,
        Phase.AllowsRtdCreation, Phase.AllowsHandleCreation,
        Phase.AllowsStateResourceCreation]
  | closing stage =>
      cases stage <;> cases hStep <;>
        simp_all [State.Certified,
        Resources.HostDetached, Resources.CallsDrained,
        Resources.ReturnsDrained, Resources.AsyncDrained,
        Resources.RtdDrained, Resources.HandlesDrained,
        Resources.StateClosed, Resources.DiagnosticsDrained,
        Resources.Quiescent,
        Phase.IsLive, Phase.AllowsReturnCreation,
        Phase.AllowsReturnFree, Phase.AllowsAsyncCreation,
        Phase.AllowsRtdCreation, Phase.AllowsHandleCreation,
        Phase.AllowsStateResourceCreation]
  | closed =>
      cases hStep <;>
        simp_all [State.Certified,
        Resources.HostDetached, Resources.CallsDrained,
        Resources.ReturnsDrained, Resources.AsyncDrained,
        Resources.RtdDrained, Resources.HandlesDrained,
        Resources.StateClosed, Resources.DiagnosticsDrained,
        Resources.Quiescent,
        Phase.IsLive, Phase.AllowsReturnCreation,
        Phase.AllowsReturnFree, Phase.AllowsAsyncCreation,
        Phase.AllowsRtdCreation, Phase.AllowsHandleCreation,
        Phase.AllowsStateResourceCreation]
  | failStopped reason =>
      cases hStep <;>
        simp_all [State.Certified,
        Resources.HostDetached, Resources.CallsDrained,
        Resources.ReturnsDrained, Resources.AsyncDrained,
        Resources.RtdDrained, Resources.HandlesDrained,
        Resources.StateClosed, Resources.DiagnosticsDrained,
        Resources.Quiescent,
        Phase.IsLive, Phase.AllowsReturnCreation,
        Phase.AllowsReturnFree, Phase.AllowsAsyncCreation,
        Phase.AllowsRtdCreation, Phase.AllowsHandleCreation,
        Phase.AllowsStateResourceCreation]

end CertifiedPreservation

/-- Reachability preserves the cumulative shutdown certificate. -/
theorem Reachable.certified
    {initial current : State}
    (hInitial : initial.Certified)
    (hReachable : Reachable initial current) :
    current.Certified := by
  induction hReachable with
  | initial =>
      exact hInitial
  | step _ hStep ih =>
      exact Step.certified_preserved ih hStep

/-- Trace-level form of certificate preservation. -/
theorem Steps.certified
    {initial final : State} {events : List Event}
    (hInitial : initial.Certified)
    (hSteps : Steps initial events final) :
    final.Certified := by
  exact Reachable.certified hInitial hSteps.reachable

/-- Reaching the finalization stage already entails full quiescence. -/
theorem reachable_finalize_is_quiescent
    {initial current : State}
    (hInitialOpen : initial.phase = .open)
    (hReachable : Reachable initial current)
    (hFinalize : current.phase = .closing .finalize) :
    current.resources.Quiescent := by
  have hCertified := Reachable.certified
    (State.certified_of_open hInitialOpen) hReachable
  simpa [State.Certified, hFinalize] using hCertified

end ExcelXllFormal.Shutdown
