import XlFnFormal.Handle.Refinement.PublishedInvariant

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Refinement

def check? (s : State) (event : Event) : Bool :=
  (apply? s event).isSome

def replay? : State → List Event → Option State
  | state, [] => some state
  | state, event :: events =>
      match apply? state event with
      | some next => replay? next events
      | none => none

inductive ReplayReachable : State → List Event → State → Prop where
  | nil (state : State) : ReplayReachable state [] state
  | cons {state next output : State} {event : Event} {events : List Event}
      (hStep : Step state event next)
      (hTail : ReplayReachable next events output) :
      ReplayReachable state (event :: events) output

theorem check?_sound
    {s : State} {event : Event}
    (h : check? s event = true) :
    ∃ s', Step s event s' := by
  unfold check? at h
  cases hApply : apply? s event with
  | none => simp [hApply] at h
  | some next => exact ⟨next, apply?_sound hApply⟩

theorem check?_complete
    {s s' : State} {event : Event}
    (h : Step s event s') :
    check? s event = true := by
  have hApply : apply? s event = some s' := apply?_complete h
  simp [check?, hApply]

theorem replay?_sound
    {s t : State} {events : List Event}
    (h : replay? s events = some t) :
    ReplayReachable s events t := by
  induction events generalizing s with
  | nil =>
      simp [replay?] at h
      cases h
      exact ReplayReachable.nil _
  | cons event events ih =>
      dsimp [replay?] at h
      split at h
      · rename_i next hApply
        have hStep : Step s event next := apply?_sound hApply
        have hTail : ReplayReachable next events t := ih h
        exact ReplayReachable.cons hStep hTail
      · contradiction

theorem replay?_complete
    {s t : State} {events : List Event}
    (h : ReplayReachable s events t) :
    replay? s events = some t := by
  induction h with
  | nil state =>
      rfl
  | @cons state next output event events hStep hTail ih =>
      have hApply : apply? state event = some next := apply?_complete hStep
      simp [replay?, hApply, ih]

theorem ReplayReachable.invariant_preserved
    {s t : State} {events : List Event}
    (hInv : s.Invariant)
    (hReach : ReplayReachable s events t) :
    t.Invariant := by
  induction hReach with
  | nil => exact hInv
  | cons hStep hTail ih =>
      exact ih (Step.invariant_preserved hInv hStep)

end XlFnFormal.Handle.Refinement
