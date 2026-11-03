import XlFnFormal.Shutdown.Safety

set_option autoImplicit false

namespace XlFnFormal.Shutdown

universe u

/-- Proof obligations that connect an implementation-level event trace to the
abstract shutdown protocol.

The Rust integration is expected to emit one `Event` at each linearization
point.  `stepSound` proves that every concrete event is admitted by `Step`;
`successIsClosed` proves that a successful `xlAutoRemove` return is represented
by the abstract successful terminal phase rather than by `failStopped`. -/
structure ShutdownRefinement (Concrete : Type u) where
  abstract : Concrete → State
  concreteStep : Concrete → Event → Concrete → Prop
  stepSound :
    ∀ {source target : Concrete} {event : Event},
      concreteStep source event target →
      Step (abstract source) event (abstract target)
  returnedSuccess : Concrete → Prop
  successIsClosed :
    ∀ {state : Concrete},
      returnedSuccess state → (abstract state).phase = .closed

/-- Concrete executions carrying the same event labels as the abstract model. -/
inductive ConcreteSteps
    {Concrete : Type u}
    (refinement : ShutdownRefinement Concrete) :
    Concrete → List Event → Concrete → Prop where
  | refl (state : Concrete) : ConcreteSteps refinement state [] state
  | cons
      {source middle target : Concrete}
      {event : Event} {events : List Event} :
      refinement.concreteStep source event middle →
      ConcreteSteps refinement middle events target →
      ConcreteSteps refinement source (event :: events) target

namespace ConcreteSteps

/-- A concrete trace satisfying the refinement obligations has a certified
abstract trace with exactly the same event labels. -/
theorem sound
    {Concrete : Type u}
    {refinement : ShutdownRefinement Concrete}
    {source target : Concrete}
    {events : List Event}
    (hSteps : ConcreteSteps refinement source events target) :
    Steps (refinement.abstract source) events (refinement.abstract target) := by
  induction hSteps with
  | refl state =>
      exact Steps.refl (refinement.abstract state)
  | cons hStep _ ih =>
      exact Steps.cons (refinement.stepSound hStep) ih

end ConcreteSteps

/-- End-to-end refinement theorem.

Once the recorded Rust event trace satisfies `ShutdownRefinement`, every
successful concrete shutdown return is proved to have no registration, active
call, DLL-owned return block, in-flight `xlAutoFree12`, async task/executor,
RTD resource, handle operation/value, escaped Add-in state, or diagnostic
dispatcher left alive. -/
theorem concrete_successful_shutdown_is_quiescent
    {Concrete : Type u}
    {refinement : ShutdownRefinement Concrete}
    {initial final : Concrete}
    {events : List Event}
    (hInitialOpen : (refinement.abstract initial).phase = .open)
    (hSteps : ConcreteSteps refinement initial events final)
    (hSuccess : refinement.returnedSuccess final) :
    (refinement.abstract final).resources.Quiescent := by
  exact hSteps.sound.successful_shutdown_is_quiescent
    hInitialOpen (refinement.successIsClosed hSuccess)

end XlFnFormal.Shutdown
