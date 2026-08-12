import XlFnFormal.Lifecycle.Trace

set_option autoImplicit false

namespace XlFnFormal.Lifecycle

theorem Step.wellFormed_preserved
    {s t : State} {event : Event}
    (hWF : s.WellFormed)
    (hStep : Step s event t) :
    t.WellFormed := by
  cases hStep <;>
    cases hSrcPhase : s.phase <;>
    cases hSrcOwner : s.cleanupOwner <;>
    (try cases ‹CleanupOwner›) <;>
    simp_all [State.WellFormed, State.AttemptOwnerDisjoint,
      State.PhaseConsistent, State.OwnerConsistent,
      State.CanBeginOpen, phaseAfterFinalClose]

theorem Reachable.wellFormed
    {initial current : State}
    (hInitial : initial.WellFormed)
    (hReachable : Reachable initial current) :
    current.WellFormed := by
  induction hReachable with
  | initial =>
      exact hInitial
  | step _ hStep ih =>
      exact Step.wellFormed_preserved ih hStep

theorem Steps.wellFormed
    {initial final : State} {events : List Event}
    (hInitial : initial.WellFormed)
    (hSteps : Steps initial events final) :
    final.WellFormed :=
  Reachable.wellFormed hInitial hSteps.reachable

end XlFnFormal.Lifecycle
