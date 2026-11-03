import XlFnFormal.Lifecycle.Trace

set_option autoImplicit false

namespace XlFnFormal.Lifecycle

theorem Step.identifierValid_preserved
    {s t : State} {event : Event}
    (hValid : s.IdentifierValid)
    (hStep : Step s event t) :
    t.IdentifierValid := by
  cases hStep <;>
    cases hPhase : s.phase <;>
    simp_all [State.IdentifierValid, phaseAfterFinalClose]

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

theorem Step.valid_preserved
    {s t : State} {event : Event}
    (hValid : s.Valid)
    (hStep : Step s event t) :
    t.Valid := by
  exact ⟨
    Step.wellFormed_preserved hValid.1 hStep,
    Step.identifierValid_preserved hValid.2 hStep
  ⟩

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

end XlFnFormal.Lifecycle
