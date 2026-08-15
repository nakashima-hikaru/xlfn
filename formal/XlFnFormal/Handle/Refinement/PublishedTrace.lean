import XlFnFormal.Handle.Refinement.PublishedSafety
import XlFnFormal.Handle.Topics.Trace

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Refinement

open XlFnFormal.Handle.Topics

def fixtureToken : Registry.Token :=
  { session := 0, slot := 0, generation := 1 }

def publishedTopicsPrefix : List Topics.Event :=
  [.beginPrepare,
   .beginInitializer fixtureKey 1,
   .insertPendingFresh fixtureKey 1,
   .publishVisible fixtureKey 1 fixtureRtdKey,
   .commitPublication fixtureKey 1,
   .finishInitializer fixtureKey 1]

def publishedTopics : Topics.State :=
  match Topics.replay? (Topics.initialState 0) publishedTopicsPrefix with
  | some state => state
  | none => Topics.initialState 0

def generationTopicsPrefix : List Topics.Event :=
  [.beginPrepare,
   .beginInitializer fixtureKey 1,
   .insertPendingFresh fixtureKey 1,
   .publishVisible fixtureKey 1 fixtureRtdKey,
   .claimServer fixtureKey 1,
   .commitPublication fixtureKey 1,
   .finishInitializer fixtureKey 1]

def generationTopics : Topics.State :=
  match Topics.replay? (Topics.initialState 0) generationTopicsPrefix with
  | some state => state
  | none => Topics.initialState 0

def warmSuccessTrace : List Event :=
  [.installProvisional fixtureKey fixtureToken fixtureRtdKey,
   .activatePublication fixtureKey fixtureToken,
   .beginWarmRead 1 fixtureKey,
   .finishWarmRead 1]

def disconnectWarmTrace : List Event :=
  [.installProvisional fixtureKey fixtureToken fixtureRtdKey,
   .activatePublication fixtureKey fixtureToken,
   .beginWarmRead 1 fixtureKey,
   .invalidatePublication fixtureKey fixtureToken]

def generationTerminationWarmTrace : List Event :=
  [.installProvisional fixtureKey fixtureToken fixtureRtdKey,
   .activatePublication fixtureKey fixtureToken,
   .beginWarmRead 1 fixtureKey,
   .invalidatePublication fixtureKey fixtureToken]

def closeWarmTrace : List Event :=
  [.installProvisional fixtureKey fixtureToken fixtureRtdKey,
   .activatePublication fixtureKey fixtureToken,
   .beginWarmRead 1 fixtureKey,
   .closePublications,
   .abandonWarmRead 1,
   .registryClose]

def warmSuccessCertified? : Bool :=
  match replay? (initialState publishedTopics) warmSuccessTrace with
  | some state =>
      (state.findSnapshot? fixtureKey).isSome &&
      state.warmReads = [] &&
      (state.findPublication? fixtureKey fixtureToken).isSome
  | none => false

def closeWarmCertified? : Bool :=
  match replay? (initialState publishedTopics) closeWarmTrace with
  | some state =>
      state.snapshot = [] && state.warmReads = [] &&
      (state.findPublication? fixtureKey fixtureToken).isSome
  | none => false

theorem warm_success_trace_replays :
    warmSuccessCertified? = true := by
  native_decide

theorem invalidated_warm_reader_cannot_succeed_trace :
    (replay? (initialState publishedTopics) disconnectWarmTrace).bind
      (fun state => apply? state (.finishWarmRead 1)) = none := by
  native_decide

theorem close_warm_reader_cannot_succeed_trace :
    (replay? (initialState publishedTopics)
      [.installProvisional fixtureKey fixtureToken fixtureRtdKey,
       .activatePublication fixtureKey fixtureToken,
       .beginWarmRead 1 fixtureKey,
       .closePublications]).bind
      (fun state => apply? state (.finishWarmRead 1)) = none := by
  native_decide

theorem close_warm_trace_replays :
    closeWarmCertified? = true := by
  native_decide

theorem terminated_generation_warm_reader_cannot_succeed_trace :
    (replay? (initialState generationTopics) generationTerminationWarmTrace).bind
      (fun state => apply? state (.finishWarmRead 1)) = none := by
  native_decide

def oldPublicationToken : Registry.Token :=
  { session := 0, slot := 0, generation := 1 }

def replacementPublicationToken : Registry.Token :=
  { session := 0, slot := 0, generation := 2 }

def abaState : State :=
  { topics := Topics.initialState 0
    publications :=
      [{ key := fixtureKey, token := oldPublicationToken, rtdKey := fixtureRtdKey,
         state := .stale },
       { key := fixtureKey, token := replacementPublicationToken,
         rtdKey := fixtureRtdKey, state := .live }]
    snapshot := [{ key := fixtureKey, token := replacementPublicationToken }]
    warmReads :=
      [{ id := 1, key := fixtureKey, token := oldPublicationToken,
         rtdKey := fixtureRtdKey }] }

theorem stale_reader_cannot_follow_replacement_publication_trace :
    apply? abaState (.finishWarmRead 1) = none := by
  native_decide

end XlFnFormal.Handle.Refinement
