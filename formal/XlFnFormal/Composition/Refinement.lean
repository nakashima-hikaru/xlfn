import XlFnFormal.Composition.Safety

set_option autoImplicit false

namespace XlFnFormal.Composition

universe u

/-- Proof obligations connecting an implementation state machine to the
    lifecycle/Shutdown composition.  The concrete state is intentionally
    abstract here: a Rust adapter can choose its own state representation
    while exposing the same linearization-point events. -/
structure CompositionRefinement (Concrete : Type u) where
  abstract : Concrete → State
  concreteStep : Concrete → Event → Concrete → Prop
  /-- Concrete counter arithmetic must be shown not to wrap before an event
      is admitted to the logical `Nat` model. -/
  noCounterWrap : Concrete → Event → Concrete → Prop
  stepSound :
    ∀ {source target : Concrete} {event : Event},
      concreteStep source event target →
      noCounterWrap source event target →
      Step (abstract source) event (abstract target)
  returnedSuccess : Concrete → Prop
  successIsReturnSafe :
    ∀ {state : Concrete},
      returnedSuccess state →
      (abstract state).lifecycle.ReturnSafe

/-- Concrete executions carrying the same event labels as the abstract
    composition model. -/
inductive ConcreteSteps
    {Concrete : Type u}
    (refinement : CompositionRefinement Concrete) :
    Concrete → List Event → Concrete → Prop where
  | refl (state : Concrete) : ConcreteSteps refinement state [] state
  | cons
      {source middle target : Concrete}
      {event : Event} {events : List Event} :
      refinement.concreteStep source event middle →
      refinement.noCounterWrap source event middle →
      ConcreteSteps refinement middle events target →
      ConcreteSteps refinement source (event :: events) target

namespace ConcreteSteps

theorem sound
    {Concrete : Type u}
    {refinement : CompositionRefinement Concrete}
    {source target : Concrete}
    {events : List Event}
    (hSteps : ConcreteSteps refinement source events target) :
    Steps (refinement.abstract source) events (refinement.abstract target) := by
  induction hSteps with
  | refl state =>
      exact Steps.refl (refinement.abstract state)
  | cons hStep hNoCounterWrap _ ih =>
      exact Steps.cons (refinement.stepSound hStep hNoCounterWrap) ih

end ConcreteSteps

/-- The abstract composition safety theorem lifted through a concrete
    refinement.  The initial-state equality is the refinement boundary's
    explicit bridge to the executable model's initial state. -/
theorem concrete_successful_xlAutoRemove_is_safe
    {Concrete : Type u}
    {refinement : CompositionRefinement Concrete}
    {initial final : Concrete}
    {events : List Event}
    (hInitial : refinement.abstract initial = State.initialState)
    (hSteps : ConcreteSteps refinement initial events final)
    (hSuccess : refinement.returnedSuccess final) :
    (refinement.abstract final).lifecycle.ReturnSafe ∧
    (refinement.abstract final).currentShutdown = none ∧
    (refinement.abstract final).logicalQuiescenceCertified = true := by
  have hAbstractSteps := hSteps.sound
  have hReachable : Reachable State.initialState (refinement.abstract final) := by
    have hReachable' := hAbstractSteps.reachable
    simpa [hInitial] using hReachable'
  exact successful_xlAutoRemove_is_safe ⟨
    hReachable,
    refinement.successIsReturnSafe hSuccess
  ⟩

end XlFnFormal.Composition
