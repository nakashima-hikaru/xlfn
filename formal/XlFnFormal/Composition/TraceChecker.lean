import XlFnFormal.Composition.Checker
import XlFnFormal.Composition.Refinement
import Lean.Data.Json

set_option autoImplicit false

namespace XlFnFormal.Composition

open Lean

private structure WireTrace where
  initial : State
  events : Array Event
  traceTruncated : Bool
  outcome : String

private def wireRefinement : CompositionRefinement State where
  abstract := id
  concreteStep source event target := apply? source event = some target
  noCounterWrap := fun _ _ _ => True
  stepSound := by
    intro source target event h _
    exact apply?_sound h
  returnedSuccess := fun state => state.lifecycle.ReturnSafe
  successIsReturnSafe := by
    intro state h
    exact h

private def field {α : Type} [FromJson α] (json : Json) (name : String) :
    Except String α :=
  json.getObjValAs? α name

private def payloadField {α : Type} [FromJson α]
    (json : Json) (tag : String) (name : String) : Except String α := do
  field (← json.getObjVal? tag) name

private def parseFailure : Json → Except String Shutdown.Failure
  | .str "boundaryPanic" => return .boundaryPanic
  | .str "unregisterFailed" => return .unregisterFailed
  | .str "returnShutdownFailed" => return .returnShutdownFailed
  | .str "asyncShutdownFailed" => return .asyncShutdownFailed
  | .str "rtdShutdownFailed" => return .rtdShutdownFailed
  | .str "handleShutdownFailed" => return .handleShutdownFailed
  | .str "generationEscaped" => return .generationEscaped
  | .str "addinShutdownFailed" => return .addinShutdownFailed
  | .str "diagnosticsShutdownFailed" => return .diagnosticsShutdownFailed
  | .str "invariantViolation" => return .invariantViolation
  | json => throw s!"unknown shutdown failure: {json}"

private def parseCompletion : Json → Except String Shutdown.Completion
  | .str "completed" => return .completed
  | .str "canceled" => return .canceled
  | .str "failed" => return .failed
  | json => throw s!"unknown async completion: {json}"

private def parseStage : Json → Except String Shutdown.CloseStage
  | .str "drainCalls" => return .drainCalls
  | .str "drainReturns" => return .drainReturns
  | .str "drainAsync" => return .drainAsync
  | .str "stopSubscriptions" => return .stopSubscriptions
  | .str "detachHost" => return .detachHost
  | .str "closeState" => return .closeState
  | .str "drainHandles" => return .drainHandles
  | .str "stopDiagnostics" => return .stopDiagnostics
  | .str "drainRtd" => return .drainRtd
  | .str "finalize" => return .finalize
  | json => throw s!"unknown shutdown stage: {json}"

private def simpleShutdownEvent : String → Option Shutdown.Event
  | "registerFunction" => some .registerFunction
  | "unregisterFunction" => some .unregisterFunction
  | "registerEvent" => some .registerEvent
  | "unregisterEvent" => some .unregisterEvent
  | "enterExternal" => some .enterExternal
  | "leaveExternal" => some .leaveExternal
  | "enterCall" => some .enterCall
  | "leaveCall" => some .leaveCall
  | "createReturnBlock" => some .createReturnBlock
  | "beginReturnFree" => some .beginReturnFree
  | "releaseReturnBlock" => some .releaseReturnBlock
  | "endReturnFree" => some .endReturnFree
  | "startAsyncExecutor" => some .startAsyncExecutor
  | "startAsyncTask" => some .startAsyncTask
  | "stopAsyncExecutor" => some .stopAsyncExecutor
  | "beginRtdOperation" => some .beginRtdOperation
  | "endRtdOperation" => some .endRtdOperation
  | "addSubscription" => some .addSubscription
  | "removeSubscription" => some .removeSubscription
  | "beginCallback" => some .beginCallback
  | "endCallback" => some .endCallback
  | "addRtdClassFactory" => some .addRtdClassFactory
  | "removeRtdClassFactory" => some .removeRtdClassFactory
  | "addRtdServer" => some .addRtdServer
  | "removeRtdServer" => some .removeRtdServer
  | "lockRtdServer" => some .lockRtdServer
  | "unlockRtdServer" => some .unlockRtdServer
  | "addHandle" => some .addHandle
  | "removeHandle" => some .removeHandle
  | "addHandlePin" => some .addHandlePin
  | "removeHandlePin" => some .removeHandlePin
  | "startDiagnostics" => some .startDiagnostics
  | "enqueueDiagnostic" => some .enqueueDiagnostic
  | "flushDiagnostic" => some .flushDiagnostic
  | "discardDiagnostic" => some .discardDiagnostic
  | "stopDiagnostics" => some .stopDiagnostics
  | "recordCleanupIssue" => some .recordCleanupIssue
  | "beginClose" => some .beginClose
  | "callsDrained" => some .callsDrained
  | "returnsDrained" => some .returnsDrained
  | "asyncDrained" => some .asyncDrained
  | "subscriptionsDrained" => some .subscriptionsDrained
  | "closeCallbackGate" => some .closeCallbackGate
  | "hostDetached" => some .hostDetached
  | "proveGenerationUnique" => some .proveGenerationUnique
  | "proveAddinQuiesced" => some .proveAddinQuiesced
  | "generationReclaimed" => some .generationReclaimed
  | "handlesDrained" => some .handlesDrained
  | "diagnosticsDrained" => some .diagnosticsDrained
  | "rtdDrained" => some .rtdDrained
  | "finishClose" => some .finishClose
  | _ => none

private def parseShutdownEvent : Json → Except String Shutdown.Event
  | json =>
      match json.getTag? with
      | some "endAsyncTask" => do
          return .endAsyncTask
            (← parseCompletion (← json.getObjVal? "endAsyncTask"))
      | some "failStop" => do
          return .failStop (← parseFailure (← json.getObjVal? "failStop"))
      | some "quarantine" => do
          return .quarantine (← parseFailure (← json.getObjVal? "quarantine"))
      | some tag =>
          match simpleShutdownEvent tag with
          | some event => return event
          | none => throw s!"unknown shutdown event: {tag}"
      | none => throw s!"shutdown event must be a string or tagged object: {json}"

private def parseResources (json : Json) : Except String Shutdown.Resources := do
  let ingressOpen : Bool ← field json "ingressOpen"
  let externalEntries : Nat ← field json "externalEntries"
  let registrations : Nat ← field json "registrations"
  let eventRegistrations : Nat ← field json "eventRegistrations"
  let registrationStateKnown : Bool ← field json "registrationStateKnown"
  let callbackGateOpen : Bool ← field json "callbackGateOpen"
  let activeCalls : Nat ← field json "activeCalls"
  let returnBlocks : Nat ← field json "returnBlocks"
  let returnBlocksInFree : Nat ← field json "returnBlocksInFree"
  let _returnFreeOperations : Nat ← field json "returnFreeOperations"
  let asyncTasks : Nat ← field json "asyncTasks"
  let asyncExecutorRunning : Bool ← field json "asyncExecutorRunning"
  let rtdOperations : Nat ← field json "rtdOperations"
  let subscriptions : Nat ← field json "subscriptions"
  let callbacks : Nat ← field json "callbacks"
  let rtdClassFactories : Nat ← field json "rtdClassFactories"
  let rtdServers : Nat ← field json "rtdServers"
  let rtdServerLocks : Nat ← field json "rtdServerLocks"
  let handles : Nat ← field json "handles"
  let handlePins : Nat ← field json "handlePins"
  let generationUnique : Bool ← field json "generationUnique"
  let addinQuiesced : Bool ← field json "addinQuiesced"
  let generationOwnedByRuntime : Bool ← field json "generationOwnedByRuntime"
  let diagnosticsPending : Nat ← field json "diagnosticsPending"
  let diagnosticsRunning : Bool ← field json "diagnosticsRunning"
  let cleanupIssues : Nat ← field json "cleanupIssues"
  return {
    ingressOpen,
    externalEntries,
    registrations,
    eventRegistrations,
    registrationStateKnown,
    callbackGateOpen,
    activeCalls,
    returnBlocks,
    returnBlocksInFree,
    returnFreeOperations := _returnFreeOperations,
    asyncTasks,
    asyncExecutorRunning,
    rtdOperations,
    subscriptions,
    callbacks,
    rtdClassFactories,
    rtdServers,
    rtdServerLocks,
    handles,
    handlePins,
    generationUnique,
    addinQuiesced,
    generationOwnedByRuntime,
    diagnosticsPending,
    diagnosticsRunning,
    cleanupIssues
  }

private def parseCompositionEvent : Json → Except String Event
  | json =>
      match json.getTag? with
      | some "beginOpen" => do
          let payload ← json.getObjVal? "beginOpen"
          return .beginOpen (← field payload "sampledEpoch") (← field payload "attempt")
      | some "finishOpenRejectedByClose" =>
          return .finishOpenRejectedByClose
            (← payloadField json "finishOpenRejectedByClose" "attempt")
      | some "failOpen" =>
          return .failOpen (← payloadField json "failOpen" "attempt")
      | some "commitOpen" => do
          let payload ← json.getObjVal? "commitOpen"
          return .commitOpen (← field payload "attempt")
            (← parseResources (← payload.getObjVal? "resources"))
      | some "liftShutdown" =>
          return .liftShutdown
            (← parseShutdownEvent (← json.getObjVal? "liftShutdown"))
      | some "finishUncommittedFinalClose" =>
          return .finishUncommittedFinalClose
            (← parseResources (← json.getObjVal? "finishUncommittedFinalClose"))
      | some "finishOpenRollback" =>
          return .finishOpenRollback
            (← parseResources (← json.getObjVal? "finishOpenRollback"))
      | some tag =>
          match tag with
          | "requestFinalClose" => return .requestFinalClose
          | "acquireFinalCloseOwner" => return .acquireFinalCloseOwner
          | "acquireOpenRollbackOwner" => return .acquireOpenRollbackOwner
          | "finishCommittedShutdown" => return .finishCommittedShutdown
          | "publishCommittedClosed" => return .publishCommittedClosed
          | "retireCommittedShutdown" => return .retireCommittedShutdown
          | "releaseCleanupOwner" => return .releaseCleanupOwner
          | tag => throw s!"unknown composition event: {tag}"
      | none =>
          throw s!"composition event must be a string or tagged object: {json}"

private def parseInitial : Json → Except String State
  | .str "initial" => return State.initialState
  | json => throw s!"composition trace initial must be \"initial\": {json}"

private def parseTrace (json : Json) : Except String WireTrace := do
  let eventsJson : Array Json ← field json "events"
  return {
    initial := (← parseInitial (← json.getObjVal? "initial"))
    events := (← eventsJson.mapM parseCompositionEvent)
    traceTruncated := (← field json "trace_truncated")
    outcome := (← field json "outcome")
  }

private def checkEventWithProof (current : State) (event : Event) :
    Except String (Subtype fun next => wireRefinement.concreteStep current event next) := do
  match h : apply? current event with
  | none => throw "event is rejected by the formal composition transition"
  | some next =>
      have hStep : wireRefinement.concreteStep current event next := by
        exact h
      return ⟨next, hStep⟩

private def replayEvents :
    (events : List Event) →
    (current : State) →
    Except String (Subtype fun final =>
      ConcreteSteps wireRefinement current events final)
  | [], current => return ⟨current, .refl current⟩
  | event :: rest, current => do
      let step ← checkEventWithProof current event
      let ⟨middle, hStep⟩ := step
      let result ← replayEvents rest middle
      let ⟨final, hRest⟩ := result
      return ⟨final, .cons hStep trivial hRest⟩

private def checkReturnedSuccess
    {initial final : State}
    {events : List Event}
    (hInitial : initial = State.initialState)
    (hSteps : ConcreteSteps wireRefinement initial events final) :
    Except String Unit := by
  if hPhase : final.lifecycle.phase = .closed then
    if hNoAttempt : final.lifecycle.openAttempt = none then
      if hNoOwner : final.lifecycle.cleanupOwner = none then
        have hSafe : final.lifecycle.ReturnSafe :=
          ⟨hPhase, hNoAttempt, hNoOwner⟩
        let _hSafety := concrete_successful_xlAutoRemove_is_safe
          hInitial hSteps hSafe
        exact .ok ()
      else
        exact .error "returned-success trace retains a cleanup owner"
    else
      exact .error "returned-success trace retains an open attempt"
  else
    exact .error "returned-success trace does not reach ReturnSafe"

private def checkTrace (json : Json) : Except String Unit := do
  let trace ← parseTrace json
  if trace.traceTruncated then
    throw "composition trace exceeded its in-memory event budget"
  let replayed ← replayEvents trace.events.toList State.initialState
  let ⟨_final, hSteps⟩ := replayed
  match trace.outcome with
  | "in_progress" => return ()
  | "returned_success" | "returnedSuccess" =>
      checkReturnedSuccess rfl hSteps
  | outcome => throw s!"unknown composition trace outcome: {outcome}"

def main : IO UInt32 := do
  let stdin ← IO.getStdin
  let input ← stdin.readToEnd
  match Json.parse input >>= checkTrace with
  | .ok _ =>
      IO.println "valid composition trace"
      return 0
  | .error message =>
      IO.eprintln s!"invalid composition trace: {message}"
      return 1

end XlFnFormal.Composition

def main : IO UInt32 := XlFnFormal.Composition.main
