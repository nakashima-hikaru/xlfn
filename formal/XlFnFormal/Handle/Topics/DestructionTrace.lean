import XlFnFormal.Handle.Topics.DestructionSafety
import XlFnFormal.Handle.Topics.Trace

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Topics

inductive MixedEvent where
  | topic (event : Event)
  | destruction (event : DestructionEvent)
deriving DecidableEq, Repr

inductive MixedStep : State → MixedEvent → State → Prop where
  | topic {s s' : State} {event : Event}
      (hStep : Step s event s') :
      MixedStep s (.topic event) s'
  | destruction {s s' : State} {event : DestructionEvent}
      (hStep : DestructionStep s event s') :
      MixedStep s (.destruction event) s'

inductive MixedReachable : State → State → Prop where
  | refl (s : State) : MixedReachable s s
  | tail {s t u : State} {event : MixedEvent} :
      MixedReachable s t → MixedStep t event u → MixedReachable s u

def applyMixed? (s : State) (event : MixedEvent) : Option State :=
  match event with
  | .topic event => apply? s event
  | .destruction event => applyDestruction? s event

theorem applyMixed?_sound
    {s s' : State} {event : MixedEvent}
    (h : applyMixed? s event = some s') :
    MixedStep s event s' := by
  cases event with
  | topic event => exact MixedStep.topic (apply?_sound h)
  | destruction event => exact MixedStep.destruction (applyDestruction?_sound h)

theorem applyMixed?_complete
    {s s' : State} {event : MixedEvent}
    (h : MixedStep s event s') :
    applyMixed? s event = some s' := by
  cases h with
  | topic hStep => exact apply?_complete hStep
  | destruction hStep => exact applyDestruction?_complete hStep

def replayMixed? : State → List MixedEvent → Option State
  | state, [] => some state
  | state, event :: events =>
      match applyMixed? state event with
      | some next => replayMixed? next events
      | none => none

theorem mixed_reachable_append
    {s t u : State} (hST : MixedReachable s t) (hTU : MixedReachable t u) :
    MixedReachable s u := by
  induction hTU with
  | refl => exact hST
  | tail hPrev hStep ih => exact MixedReachable.tail ih hStep

theorem replayMixed?_sound
    {s t : State} {events : List MixedEvent}
    (h : replayMixed? s events = some t) :
    MixedReachable s t := by
  induction events generalizing s with
  | nil =>
      simp [replayMixed?] at h
      cases h
      exact MixedReachable.refl _
  | cons event events ih =>
      dsimp [replayMixed?] at h
      split at h
      · rename_i next hApply
        have hStep : MixedStep s event next := applyMixed?_sound hApply
        have hTail : MixedReachable next t := ih h
        exact mixed_reachable_append
          (MixedReachable.tail (MixedReachable.refl s) hStep) hTail
      · contradiction

def noLiveSlots? : List Registry.SlotState → Bool
  | [] => true
  | .live _ :: _ => false
  | _ :: slots => noLiveSlots? slots

theorem noLiveSlots?_sound
    {slots : List Registry.SlotState}
    (h : noLiveSlots? slots = true) :
    ∀ slot (hInBounds : slot < slots.length),
      ¬ (slots.get ⟨slot, hInBounds⟩).IsLive := by
  induction slots with
  | nil =>
      intro slot hInBounds
      simp at hInBounds
  | cons head tail ih =>
      cases head with
      | live generation =>
          simp [noLiveSlots?] at h
      | vacant generation =>
          intro slot hInBounds
          cases slot with
          | zero => simp [Registry.SlotState.IsLive]
          | succ slot =>
              exact ih (by simpa [noLiveSlots?] using h) slot
                (by simpa using hInBounds)
      | retired =>
          intro slot hInBounds
          cases slot with
          | zero => simp [Registry.SlotState.IsLive]
          | succ slot =>
              exact ih (by simpa [noLiveSlots?] using h) slot
                (by simpa using hInBounds)

def replayCertified? (events : List MixedEvent) : Bool :=
  match replayMixed? (initialState 0) events with
  | some s =>
      if h : s.runtime.phase = .closed ∧
          s.runtime.activePrepares = 0 ∧ s.runtime.initializers = [] ∧
          s.runtime.registry.closed = true ∧
          s.runtime.registry.activeLeases = 0 ∧
          noLiveSlots? s.runtime.registry.slots = true ∧
          s.byKey = [] ∧ s.byRtdKey = [] ∧ s.byExcelOwner = [] ∧
          s.initializing = [] ∧ s.detached = [] then true else false
  | none => false

theorem replay_close_certified_of_check
    {events : List MixedEvent}
    (hCheck : replayCertified? events = true) :
    ∃ s, replayMixed? (initialState 0) events = some s ∧
      s.runtime.phase = .closed ∧ CloseCertified s := by
  unfold replayCertified? at hCheck
  generalize hReplay : replayMixed? (initialState 0) events = output at hCheck
  cases output with
  | none => simp [hReplay] at hCheck
  | some s =>
      have hFields :
          s.runtime.phase = .closed ∧
          s.runtime.activePrepares = 0 ∧ s.runtime.initializers = [] ∧
          s.runtime.registry.closed = true ∧
          s.runtime.registry.activeLeases = 0 ∧
          noLiveSlots? s.runtime.registry.slots = true ∧
          s.byKey = [] ∧ s.byRtdKey = [] ∧ s.byExcelOwner = [] ∧
          s.initializing = [] ∧ s.detached = [] := by
        simpa [hReplay] using hCheck
      rcases hFields with ⟨hPhase, hPrepares, hInitializers, hClosed,
        hLeases, hNoLive, hByKey, hByRtdKey, hByOwner, hInitializing,
        hDetached⟩
      refine ⟨s, rfl, hPhase, ?_⟩
      exact ⟨
        ⟨hPhase, hPrepares, hInitializers,
          ⟨hClosed, hLeases, noLiveSlots?_sound hNoLive⟩⟩,
        hByKey, hByRtdKey, hByOwner, hInitializing, hDetached⟩

def fixtureToken : Registry.Token :=
  { session := 0, slot := 0, generation := 1 }

def disconnect_pending_prefix : List MixedEvent :=
  [.topic .beginPrepare,
   .topic (.beginInitializer fixtureKey 1),
   .topic (.insertPendingFresh fixtureKey 1),
   .topic (.publishVisible fixtureKey 1 fixtureRtdKey),
   .topic (.beginConnection fixtureKey fixtureOwner1),
   .destruction (.disconnectTopic fixtureKey fixtureOwner1)]

def disconnect_pending_trace : List MixedEvent :=
  disconnect_pending_prefix ++
    [.destruction (.drainPendingReuse fixtureToken 1 2),
     .topic (.finishInitializer fixtureKey 1),
     .topic .endPrepare,
     .topic .sealTopics,
     .topic .closeRegistry,
     .topic .finishClose]

def disconnect_published_trace : List MixedEvent :=
  [.topic .beginPrepare,
   .topic (.beginInitializer fixtureKey 1),
   .topic (.insertPendingFresh fixtureKey 1),
   .topic (.publishVisible fixtureKey 1 fixtureRtdKey),
   .topic (.commitPublication fixtureKey 1),
   .topic (.finishInitializer fixtureKey 1),
   .topic (.beginConnection fixtureKey fixtureOwner1),
   .topic (.commitConnection fixtureKey fixtureOwner1),
   .destruction (.disconnectTopic fixtureKey fixtureOwner1),
   .destruction (.drainPublishedReuse fixtureToken 2),
   .topic .endPrepare,
   .topic .sealTopics,
   .topic .closeRegistry,
   .topic .finishClose]

theorem disconnect_pending_trace_replays :
    ∃ s, replayMixed? (initialState 0) disconnect_pending_trace = some s ∧
      s.runtime.phase = .closed ∧ CloseCertified s := by
  apply replay_close_certified_of_check
  native_decide

theorem published_drain_cannot_remove_pending_root :
    (replayMixed? (initialState 0) disconnect_pending_prefix).bind
      (fun s => applyDestruction? s
        (.drainPublishedReuse fixtureToken 2)) = none := by
  native_decide

theorem disconnect_published_trace_replays :
    ∃ s, replayMixed? (initialState 0) disconnect_published_trace = some s ∧
      s.runtime.phase = .closed ∧ CloseCertified s := by
  apply replay_close_certified_of_check
  native_decide

end XlFnFormal.Handle.Topics
