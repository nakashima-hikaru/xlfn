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

def fixtureRtdKey : RtdKey := "fixture-rtd"

def close_suffix : List Event :=
  [.endPrepare, .sealTopics, .closeRegistry, .finishClose]

def sealed_close_suffix : List Event :=
  [.endPrepare, .closeRegistry, .finishClose]

def success_trace : List Event :=
  [.beginPrepare,
   .beginInitializer fixtureKey 1,
   .insertPendingFresh fixtureKey 1,
   .publishVisible fixtureKey 1 fixtureRtdKey,
   .commitPublication fixtureKey 1,
   .finishInitializer fixtureKey 1] ++ close_suffix

def seal_before_visible_rollback_trace : List Event :=
  [.beginPrepare,
   .beginInitializer fixtureKey 1,
   .sealTopics,
   .insertPendingFresh fixtureKey 1,
   .rollbackPendingReuse fixtureKey 1 2,
   .finishInitializer fixtureKey 1] ++ sealed_close_suffix

def seal_after_visible_rollback_trace : List Event :=
  [.beginPrepare,
   .beginInitializer fixtureKey 1,
   .insertPendingFresh fixtureKey 1,
   .publishVisible fixtureKey 1 fixtureRtdKey,
   .sealTopics,
   .rollbackPendingReuse fixtureKey 1 2,
   .finishInitializer fixtureKey 1] ++ sealed_close_suffix

def rollback_trace : List Event := seal_before_visible_rollback_trace

theorem replay_close_certified
    {session : Registry.SessionId} {events : List Event} {s : State}
    (hReplay : replay? (initialState session) events = some s)
    (hClosed : s.runtime.phase = .closed) :
    CloseCertified s :=
  successful_close_is_certified (replay?_sound hReplay) hClosed

theorem replay_phase_is_closed
    {session : Registry.SessionId} {events : List Event}
    (hPhase : (replay? (initialState session) events).map
      (fun s => s.runtime.phase) = some .closed) :
    ∃ s, replay? (initialState session) events = some s ∧
      s.runtime.phase = .closed ∧
      CloseCertified s := by
  generalize hOut : replay? (initialState session) events = output at hPhase
  cases output with
  | none => simp at hPhase
  | some s =>
      have hClosed : s.runtime.phase = .closed := by
        simpa using hPhase
      exact ⟨s, rfl, hClosed, replay_close_certified hOut hClosed⟩

theorem success_trace_replays :
    ∃ s, replay? (initialState 0) success_trace = some s ∧
      s.runtime.phase = .closed ∧ CloseCertified s := by
  apply replay_phase_is_closed
  native_decide

theorem seal_before_visible_rollback_trace_replays :
    ∃ s, replay? (initialState 0) seal_before_visible_rollback_trace = some s ∧
      s.runtime.phase = .closed ∧ CloseCertified s := by
  apply replay_phase_is_closed
  native_decide

theorem seal_after_visible_rollback_trace_replays :
    ∃ s, replay? (initialState 0) seal_after_visible_rollback_trace = some s ∧
      s.runtime.phase = .closed ∧ CloseCertified s := by
  apply replay_phase_is_closed
  native_decide

end XlFnFormal.Handle.Topics
