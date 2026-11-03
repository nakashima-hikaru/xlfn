import XlFnFormal.Composition.Transition

set_option autoImplicit false

namespace XlFnFormal.Composition

inductive Steps : State → List Event → State → Prop where
  | refl (s : State) : Steps s [] s
  | cons {s t u : State} {event : Event} {events : List Event} :
      Step s event t →
      Steps t events u →
      Steps s (event :: events) u

inductive Reachable (initial : State) : State → Prop where
  | initial : Reachable initial initial
  | step {s t : State} {event : Event} :
      Reachable initial s →
      Step s event t →
      Reachable initial t

namespace Steps

theorem append
    {s t u : State}
    {left right : List Event}
    (hLeft : Steps s left t)
    (hRight : Steps t right u) :
    Steps s (left ++ right) u := by
  induction hLeft generalizing u right with
  | refl =>
      simpa using hRight
  | cons hStep _ ih =>
      simpa using Steps.cons hStep (ih hRight)

theorem toReachable
    {initial source target : State}
    {events : List Event}
    (hSteps : Steps source events target)
    (hSource : Reachable initial source) :
    Reachable initial target := by
  induction hSteps generalizing initial with
  | refl =>
      exact hSource
  | cons hStep _ ih =>
      exact ih (Reachable.step hSource hStep)

theorem reachable
    {source target : State}
    {events : List Event}
    (hSteps : Steps source events target) :
    Reachable source target :=
  hSteps.toReachable Reachable.initial

end Steps

end XlFnFormal.Composition
