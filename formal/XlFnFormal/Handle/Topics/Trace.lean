import XlFnFormal.Handle.Topics.Checker

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Topics

def replay? : State → List Event → Option State
  | state, [] => some state
  | state, event :: events =>
      match apply? state event with
      | some next => replay? next events
      | none => none

theorem reachable_append
    {s t u : State} (hST : Reachable s t) (hTU : Reachable t u) :
    Reachable s u := by
  induction hTU with
  | refl => exact hST
  | tail hPrev hStep ih => exact Reachable.tail ih hStep

theorem replay?_sound
    {s t : State} {events : List Event}
    (h : replay? s events = some t) :
    Reachable s t := by
  induction events generalizing s with
  | nil =>
      simp [replay?] at h
      cases h
      exact Reachable.refl _
  | cons event events ih =>
      dsimp [replay?] at h
      split at h
      · rename_i next hApply
        have hStep : Step s event next := apply?_sound hApply
        have hTail : Reachable next t := ih h
        exact reachable_append (Reachable.tail (Reachable.refl s) hStep) hTail
      · contradiction

def fixtureKey : TopicKey :=
  { sheetId := 0, row := 0, column := 0, udfId := "fixture", argumentDigest := 0 }

/-! The two named traces are the H3.1 fixture vocabulary.  Their JSON
    counterparts live under `formal/fixtures/topics/`; the checker above is the
    executable replay boundary used by future producer integration. -/
def success_trace : List Event :=
  [.beginPrepare,
   .beginInitializer fixtureKey 1,
   .publishVisibleFresh fixtureKey 1,
   .commitPublication fixtureKey 1,
   .finishInitializer fixtureKey 1]

def rollback_trace : List Event :=
  [.beginPrepare,
   .beginInitializer fixtureKey 1,
   .sealTopics,
   .publishVisibleFresh fixtureKey 1,
   .rollbackVisibleReuse fixtureKey 1 2,
   .finishInitializer fixtureKey 1,
   .endPrepare]

end XlFnFormal.Handle.Topics
