import XlFnFormal.Handle.Refinement.PublishedInvariant
import XlFnFormal.Handle.Refinement.PublishedSafety
import XlFnFormal.Handle.Topics.Trace

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Refinement

open XlFnFormal.Handle.Topics

def fixtureToken : Registry.Token :=
  { session := 0, slot := 0, generation := 1 }

def replacementPublicationToken : Registry.Token :=
  { session := 0, slot := 0, generation := 2 }

def publishedPrefix : List Event :=
   [.topic .beginPrepare,
    .topic (.beginInitializer fixtureKey 1),
    .topic (.insertPendingFresh fixtureKey 1),
   .publishAndInstallProvisional fixtureKey 1 fixtureToken fixtureRtdKey,
   .commitAndActivate fixtureKey 1 fixtureToken,
   .topic (.finishInitializer fixtureKey 1),
   .topic .endPrepare]

def publishedWithConnectionPrefix : List Event :=
  publishedPrefix ++
    [.topic (.beginConnection fixtureKey fixtureOwner1),
     .topic (.commitConnection fixtureKey fixtureOwner1)]

def warmSuccessTrace : List Event :=
  publishedPrefix ++
    [.topic .beginPrepare,
     .beginWarmRead 1 fixtureKey,
     .finishWarmRead 1,
     .topic .endPrepare]

def failWarmReadTrace : List Event :=
  publishedPrefix ++
    [.topic .beginPrepare,
     .beginWarmRead 1 fixtureKey,
     .failWarmRead 1,
     .topic .endPrepare]

def coldObserveFailureTrace : List Event :=
  [.topic .beginPrepare,
   .topic (.beginInitializer fixtureKey 1),
   .topic (.insertPendingFresh fixtureKey 1),
   .publishAndInstallProvisional fixtureKey 1 fixtureToken fixtureRtdKey,
   .withdrawAndInvalidate fixtureKey 1 fixtureToken,
   .topic (.rollbackPendingReuse fixtureKey 1 2),
   .topic (.finishInitializer fixtureKey 1),
   .topic .endPrepare]

def disconnectWarmPrefix : List Event :=
  publishedWithConnectionPrefix ++
    [.topic .beginPrepare,
     .beginWarmRead 1 fixtureKey,
     .disconnect fixtureKey fixtureOwner1]

def disconnectCloseTrace : List Event :=
  disconnectWarmPrefix ++
    [.abandonWarmRead 1,
     .topic .endPrepare,
     .drainPublishedReuse fixtureToken 2,
     .sealForClose,
     .closeRegistry,
     .topic .finishClose]

def generationWarmPrefix : List Event :=
  publishedPrefix ++
    [.topic (.claimServer fixtureKey 1),
     .topic .beginPrepare,
     .beginWarmRead 1 fixtureKey,
     .detachGeneration 1]

def generationTerminationCloseTrace : List Event :=
  generationWarmPrefix ++
    [.abandonWarmRead 1,
     .topic .endPrepare,
     .drainPublishedReuse fixtureToken 2,
     .sealForClose,
     .closeRegistry,
     .topic .finishClose]

def closeWarmTrace : List Event :=
  publishedPrefix ++
    [.topic .beginPrepare,
     .beginWarmRead 1 fixtureKey,
     .sealForClose,
     .abandonWarmRead 1,
     .topic .endPrepare,
     .closeRegistry,
     .topic .finishClose]

def abaTrace : List Event :=
  publishedWithConnectionPrefix ++
    [.topic .beginPrepare,
     .beginWarmRead 1 fixtureKey,
     .disconnect fixtureKey fixtureOwner1,
     .drainPublishedReuse fixtureToken 2,
     .topic .beginPrepare,
     .topic (.beginInitializer fixtureKey 2),
     .topic (.insertPendingReuse fixtureKey 2 0 2),
     .publishAndInstallProvisional fixtureKey 2 replacementPublicationToken fixtureRtdKey,
     .commitAndActivate fixtureKey 2 replacementPublicationToken,
     .topic (.finishInitializer fixtureKey 2),
     .topic .endPrepare]

def abaCloseTrace : List Event :=
  abaTrace ++
    [.abandonWarmRead 1,
     .topic .endPrepare,
     .sealForClose,
     .closeRegistry,
     .topic .finishClose]

def closeCertificate? (s : State) : Bool :=
  s.topics.runtime.phase == .closed &&
  s.topics.byKey == [] &&
  s.topics.byRtdKey == [] &&
  s.topics.byExcelOwner == [] &&
  s.topics.initializing == [] &&
  s.topics.detached == [] &&
  s.snapshot == [] &&
  s.warmReads == []

theorem replay_invariant
    {s t : State} {events : List Event}
    (hInv : s.Invariant)
    (hReplay : replay? s events = some t) :
    t.Invariant := by
  exact ReplayReachable.invariant_preserved hInv (replay?_sound hReplay)

theorem replay_close_certificate
    {events : List Event} {s : State}
    (hReplay : replay? (initialState (Topics.initialState 0)) events = some s)
    (hClosed : closeCertificate? s = true) :
    s.Invariant ∧ closeCertificate? s = true := by
  exact ⟨replay_invariant (initialInvariant (Topics.initial_invariant 0)) hReplay,
    hClosed⟩

def warmSuccessCertified? : Bool :=
  match replay? (initialState (Topics.initialState 0)) warmSuccessTrace with
  | some state =>
      state.warmReads = [] &&
      (state.findSnapshot? fixtureKey).isSome &&
      (state.findPublication? fixtureKey fixtureToken).isSome
  | none => false

def failWarmReadCertified? : Bool :=
  match replay? (initialState (Topics.initialState 0)) failWarmReadTrace with
  | some state =>
      state.warmReads = [] &&
      (state.findPublication? fixtureKey fixtureToken).isSome
  | none => false

def coldObserveFailureCertified? : Bool :=
  match replay? (initialState (Topics.initialState 0)) coldObserveFailureTrace with
  | some state =>
      state.topics.findTopic? fixtureKey == none &&
      state.topics.findReverse? fixtureRtdKey == none &&
      state.snapshot == [] &&
      (match state.findPublication? fixtureKey fixtureToken with
       | some publication => publication.state == .stale
       | none => false) &&
      state.topics.runtime.activePrepares == 0 &&
      state.topics.runtime.initializers == [] &&
      tokenLive? state.topics.runtime.registry fixtureToken == false
  | none => false

theorem cold_observation_failure_trace_replays :
    coldObserveFailureCertified? = true := by
  native_decide

def closeWarmCertified? : Bool :=
  match replay? (initialState (Topics.initialState 0)) closeWarmTrace with
  | some state => closeCertificate? state
  | none => false

def disconnectCloseCertified? : Bool :=
  match replay? (initialState (Topics.initialState 0)) disconnectCloseTrace with
  | some state => closeCertificate? state
  | none => false

def generationTerminationCloseCertified? : Bool :=
  match replay? (initialState (Topics.initialState 0)) generationTerminationCloseTrace with
  | some state => closeCertificate? state
  | none => false

theorem warm_success_trace_replays :
    warmSuccessCertified? = true := by
  native_decide

theorem warm_observation_failure_trace_replays :
    failWarmReadCertified? = true := by
  native_decide

theorem close_warm_trace_replays :
    closeWarmCertified? = true := by
  native_decide

theorem invalidated_warm_reader_cannot_succeed_trace :
    (replay? (initialState (Topics.initialState 0)) disconnectWarmPrefix).bind
      (fun state => apply? state (.finishWarmRead 1)) = none := by
  native_decide

theorem disconnect_close_trace_replays :
    disconnectCloseCertified? = true := by
  native_decide

theorem terminated_generation_warm_reader_cannot_succeed_trace :
    (replay? (initialState (Topics.initialState 0)) generationWarmPrefix).bind
      (fun state => apply? state (.finishWarmRead 1)) = none := by
  native_decide

theorem generation_termination_close_trace_replays :
    generationTerminationCloseCertified? = true := by
  native_decide

theorem stale_reader_cannot_follow_replacement_publication_trace :
    (replay? (initialState (Topics.initialState 0)) abaTrace).bind
      (fun state => apply? state (.finishWarmRead 1)) = none := by
  native_decide

theorem aba_trace_replays :
    (replay? (initialState (Topics.initialState 0)) abaTrace).isSome = true := by
  native_decide

def abaCloseCertified? : Bool :=
  match replay? (initialState (Topics.initialState 0)) abaCloseTrace with
  | some state => closeCertificate? state
  | none => false

theorem aba_close_trace_replays :
    abaCloseCertified? = true := by
  native_decide

theorem aba_trace_preserves_invariant :
    ∀ state,
      replay? (initialState (Topics.initialState 0)) abaTrace = some state →
      state.Invariant := by
  intro state hReplay
  exact replay_invariant (initialInvariant (Topics.initial_invariant 0)) hReplay

end XlFnFormal.Handle.Refinement
