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

def fixtureKey2 : TopicKey :=
  { sheetId := 0, row := 1, column := 0, udfId := "fixture-2", argumentDigest := 1 }

def fixtureRtdKey : RtdKey := "fixture-rtd"

def fixtureRtdKey2 : RtdKey := "fixture-rtd-2"

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

def observe_failure_rollback_trace : List Event :=
  [.beginPrepare,
   .beginInitializer fixtureKey 1,
   .insertPendingFresh fixtureKey 1,
   .publishVisible fixtureKey 1 fixtureRtdKey,
   .withdrawVisible fixtureKey 1,
   .rollbackPendingReuse fixtureKey 1 2,
   .finishInitializer fixtureKey 1] ++ close_suffix

def excel_connection_success_trace : List Event :=
  [.beginPrepare,
   .beginInitializer fixtureKey 1,
   .insertPendingFresh fixtureKey 1,
   .publishVisible fixtureKey 1 fixtureRtdKey,
   .beginConnection fixtureKey 7,
   .commitConnection fixtureKey 7,
   .commitPublication fixtureKey 1,
   .finishInitializer fixtureKey 1] ++ close_suffix

def excel_existing_topic_connection_trace : List Event :=
  [.beginPrepare,
   .beginInitializer fixtureKey 1,
   .insertPendingFresh fixtureKey 1,
   .publishVisible fixtureKey 1 fixtureRtdKey,
   .commitPublication fixtureKey 1,
   .finishInitializer fixtureKey 1,
   .beginConnection fixtureKey 7,
   .commitConnection fixtureKey 7] ++ close_suffix

def excel_connection_reuse_trace : List Event :=
  [.beginPrepare,
   .beginInitializer fixtureKey 1,
   .insertPendingFresh fixtureKey 1,
   .publishVisible fixtureKey 1 fixtureRtdKey,
   .beginConnection fixtureKey 7,
   .commitConnection fixtureKey 7,
   .reuseCommittedConnection fixtureKey 7,
   .commitPublication fixtureKey 1,
   .finishInitializer fixtureKey 1] ++ close_suffix

def excel_connection_rollback_trace : List Event :=
  [.beginPrepare,
   .beginInitializer fixtureKey 1,
   .insertPendingFresh fixtureKey 1,
   .publishVisible fixtureKey 1 fixtureRtdKey,
   .beginConnection fixtureKey 7,
   .rollbackConnection fixtureKey 7,
   .commitPublication fixtureKey 1,
   .finishInitializer fixtureKey 1] ++ close_suffix

def excel_observe_failure_rollback_trace : List Event :=
  [.beginPrepare,
   .beginInitializer fixtureKey 1,
   .insertPendingFresh fixtureKey 1,
   .publishVisible fixtureKey 1 fixtureRtdKey,
   .beginConnection fixtureKey 7,
   .commitConnection fixtureKey 7,
   .withdrawVisible fixtureKey 1,
   .rollbackPendingReuse fixtureKey 1 2,
   .finishInitializer fixtureKey 1] ++ close_suffix

def excel_seal_after_visible_rollback_trace : List Event :=
  [.beginPrepare,
   .beginInitializer fixtureKey 1,
   .insertPendingFresh fixtureKey 1,
   .publishVisible fixtureKey 1 fixtureRtdKey,
   .beginConnection fixtureKey 7,
   .sealTopics,
   .rollbackPendingReuse fixtureKey 1 2,
   .finishInitializer fixtureKey 1] ++ sealed_close_suffix

def excel_owner_reuse_trace : List Event :=
  [.beginPrepare,
   .beginInitializer fixtureKey 1,
   .insertPendingFresh fixtureKey 1,
   .publishVisible fixtureKey 1 fixtureRtdKey,
   .beginConnection fixtureKey 7,
   .rollbackConnection fixtureKey 7,
   .commitPublication fixtureKey 1,
   .finishInitializer fixtureKey 1,
   .beginInitializer fixtureKey2 2,
   .insertPendingFresh fixtureKey2 2,
   .publishVisible fixtureKey2 2 fixtureRtdKey2,
   .beginConnection fixtureKey2 7,
   .commitConnection fixtureKey2 7,
   .commitPublication fixtureKey2 2,
   .finishInitializer fixtureKey2 2] ++ close_suffix

def excel_provisional_connection_prefix : List Event :=
  [.beginPrepare,
   .beginInitializer fixtureKey 1,
   .insertPendingFresh fixtureKey 1,
   .publishVisible fixtureKey 1 fixtureRtdKey,
   .beginConnection fixtureKey 7]

def excel_committed_connection_prefix : List Event :=
  excel_provisional_connection_prefix ++ [.commitConnection fixtureKey 7]

def excel_owner_collision_prefix : List Event :=
  [.beginPrepare,
   .beginInitializer fixtureKey 1,
   .insertPendingFresh fixtureKey 1,
   .publishVisible fixtureKey 1 fixtureRtdKey,
   .beginConnection fixtureKey 7,
   .commitConnection fixtureKey 7,
   .commitPublication fixtureKey 1,
   .finishInitializer fixtureKey 1,
   .beginInitializer fixtureKey2 2,
   .insertPendingFresh fixtureKey2 2,
   .publishVisible fixtureKey2 2 fixtureRtdKey2]

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

theorem observe_failure_rollback_trace_replays :
    ∃ s, replay? (initialState 0) observe_failure_rollback_trace = some s ∧
      s.runtime.phase = .closed ∧ CloseCertified s := by
  apply replay_phase_is_closed
  native_decide

theorem excel_connection_success_trace_replays :
    ∃ s, replay? (initialState 0) excel_connection_success_trace = some s ∧
      s.runtime.phase = .closed ∧ CloseCertified s := by
  apply replay_phase_is_closed
  native_decide

theorem excel_connection_reuse_trace_replays :
    ∃ s, replay? (initialState 0) excel_connection_reuse_trace = some s ∧
      s.runtime.phase = .closed ∧ CloseCertified s := by
  apply replay_phase_is_closed
  native_decide

theorem excel_existing_topic_connection_trace_replays :
    ∃ s, replay? (initialState 0) excel_existing_topic_connection_trace = some s ∧
      s.runtime.phase = .closed ∧ CloseCertified s := by
  apply replay_phase_is_closed
  native_decide

theorem excel_connection_rollback_trace_replays :
    ∃ s, replay? (initialState 0) excel_connection_rollback_trace = some s ∧
      s.runtime.phase = .closed ∧ CloseCertified s := by
  apply replay_phase_is_closed
  native_decide

theorem excel_observe_failure_rollback_trace_replays :
    ∃ s, replay? (initialState 0) excel_observe_failure_rollback_trace = some s ∧
      s.runtime.phase = .closed ∧ CloseCertified s := by
  apply replay_phase_is_closed
  native_decide

theorem excel_seal_after_visible_rollback_trace_replays :
    ∃ s, replay? (initialState 0) excel_seal_after_visible_rollback_trace = some s ∧
      s.runtime.phase = .closed ∧ CloseCertified s := by
  apply replay_phase_is_closed
  native_decide

theorem excel_owner_reuse_trace_replays :
    ∃ s, replay? (initialState 0) excel_owner_reuse_trace = some s ∧
      s.runtime.phase = .closed ∧ CloseCertified s := by
  apply replay_phase_is_closed
  native_decide

theorem provisional_excel_reuse_rejected :
    (replay? (initialState 0) excel_provisional_connection_prefix).bind
      (fun s => apply? s (.reuseCommittedConnection fixtureKey 7)) = none := by
  native_decide

theorem committed_excel_reconnect_is_not_a_new_begin :
    (replay? (initialState 0) excel_committed_connection_prefix).bind
      (fun s => apply? s (.beginConnection fixtureKey 7)) = none := by
  native_decide

theorem different_owner_on_owned_topic_rejected :
    (replay? (initialState 0) excel_provisional_connection_prefix).bind
      (fun s => apply? s (.beginConnection fixtureKey 8)) = none := by
  native_decide

theorem same_owner_on_different_topic_rejected :
    (replay? (initialState 0) excel_owner_collision_prefix).bind
      (fun s => apply? s (.beginConnection fixtureKey2 7)) = none := by
  native_decide

end XlFnFormal.Handle.Topics
