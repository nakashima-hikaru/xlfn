import XlFnFormal.Shutdown.Checker
import XlFnFormal.Shutdown.Refinement
import Lean.Data.Json

set_option autoImplicit false

namespace XlFnFormal.Shutdown

open Lean

private def schemaVersion : Nat := 3

private structure WireState where
  generation : Nat
  state : State
  deriving DecidableEq

private structure WireTrace where
  schemaVersion : Nat
  generation : Nat
  initial : WireState
  events : Array Event
  traceTruncated : Bool
  outcome : String

private def wireReturnedSuccessBool (state : WireState) : Bool :=
  state.state.phase == .closed && isQuiescent state.state.resources

private def wireReturnedSuccess (state : WireState) : Prop :=
  state.state.phase = .closed ∧ isQuiescent state.state.resources = true

private def wireRefinement : ShutdownRefinement WireState where
  abstract := fun state => state.state
  concreteStep source event target :=
    source.generation = target.generation ∧ apply? source.state event = some target.state
  stepSound := by
    intro source target event h
    exact apply?_sound h.2
  returnedSuccess := wireReturnedSuccess
  successIsClosed := by
    intro state h
    exact h.1

private theorem wire_returned_success_of_bool
    {state : WireState}
    (h : wireReturnedSuccessBool state = true) :
    wireRefinement.returnedSuccess state := by
  simp [wireReturnedSuccessBool, wireRefinement, wireReturnedSuccess] at h ⊢
  exact h

private theorem wire_initial_open_of_bool
    {state : WireState}
    (h : (state.state.phase == .open) = true) :
    (wireRefinement.abstract state).phase = .open := by
  simpa [wireRefinement] using h

private theorem wire_refinement_success_is_quiescent
    {initial final : WireState}
    {events : List Event}
    (hInitialOpen : (wireRefinement.abstract initial).phase = .open)
    (hSteps : ConcreteSteps wireRefinement initial events final)
    (hSuccess : wireRefinement.returnedSuccess final) :
    (wireRefinement.abstract final).resources.Quiescent := by
  exact concrete_successful_shutdown_is_quiescent hInitialOpen hSteps hSuccess

private def field {α : Type} [FromJson α] (json : Json) (name : String) : Except String α :=
  json.getObjValAs? α name

private def parseStage : Json → Except String CloseStage
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

private def parseFailure : Json → Except String Failure
  | .str "boundaryPanic" => return .boundaryPanic
  | .str "unregisterFailed" => return .unregisterFailed
  | .str "returnShutdownFailed" => return .returnShutdownFailed
  | .str "asyncShutdownFailed" => return .asyncShutdownFailed
  | .str "rtdShutdownFailed" => return .rtdShutdownFailed
  | .str "handleShutdownFailed" => return .handleShutdownFailed
  | .str "stateEscaped" => return .stateEscaped
  | .str "addinShutdownFailed" => return .addinShutdownFailed
  | .str "diagnosticsShutdownFailed" => return .diagnosticsShutdownFailed
  | .str "invariantViolation" => return .invariantViolation
  | json => throw s!"unknown shutdown failure: {json}"

private def parseCompletion : Json → Except String Completion
  | .str "completed" => return .completed
  | .str "canceled" => return .canceled
  | .str "failed" => return .failed
  | json => throw s!"unknown async completion: {json}"

private def simpleEvent : String → Option Event
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
  | "proveStateUnique" => some .proveStateUnique
  | "proveAddinQuiesced" => some .proveAddinQuiesced
  | "stateClosed" => some .stateClosed
  | "handlesDrained" => some .handlesDrained
  | "diagnosticsDrained" => some .diagnosticsDrained
  | "rtdDrained" => some .rtdDrained
  | "finishClose" => some .finishClose
  | _ => none

private def parseEvent : Json → Except String Event
  | json =>
      match json.getTag? with
      | some "endAsyncTask" => do
          let payload ← json.getObjVal? "endAsyncTask"
          return .endAsyncTask (← parseCompletion payload)
      | some "failStop" => do
          let payload ← json.getObjVal? "failStop"
          return .failStop (← parseFailure payload)
      | some tag =>
          match simpleEvent tag with
          | some event => return event
          | none => throw s!"unknown shutdown event: {tag}"
      | none => throw s!"shutdown event must be a string or tagged object: {json}"

private def parsePhase : Json → Except String Phase
  | .str "Open" => return .open
  | .str "Closed" => return .closed
  | json@(.obj _) =>
      match json.getTag? with
      | some "Closing" => return .closing (← parseStage (← json.getObjVal? "Closing"))
      | some "FailStopped" => return .failStopped (← parseFailure (← json.getObjVal? "FailStopped"))
      | some tag => throw s!"unknown shutdown phase: {tag}"
      | none => throw s!"shutdown phase must be a tagged value: {json}"
  | json => throw s!"unknown shutdown phase: {json}"

private def parseResources (json : Json) : Except String Resources := do
  let ingressOpen : Bool ← field json "ingressOpen"
  let externalEntries : Nat ← field json "externalEntries"
  let registrations : Nat ← field json "registrations"
  let eventRegistrations : Nat ← field json "eventRegistrations"
  let registrationStateKnown : Bool ← field json "registrationStateKnown"
  let callbackGateOpen : Bool ← field json "callbackGateOpen"
  let activeCalls : Nat ← field json "activeCalls"
  let returnBlocks : Nat ← field json "returnBlocks"
  let returnBlocksInFree : Nat ← field json "returnBlocksInFree"
  let returnFreeOperations : Nat ← field json "returnFreeOperations"
  let asyncTasks : Nat ← field json "asyncTasks"
  let asyncExecutorRunning : Bool ← field json "asyncExecutorRunning"
  let rtdOperations : Nat ← field json "rtdOperations"
  let subscriptions : Nat ← field json "subscriptions"
  let callbacks : Nat ← field json "callbacks"
  let rtdClassFactories : Nat ← field json "rtdClassFactories"
  let rtdServers : Nat ← field json "rtdServers"
  let rtdServerLocks : Nat ← field json "rtdServerLocks"
  let handles : Nat ← field json "handles"
  let stateUnique : Bool ← field json "stateUnique"
  let addinQuiesced : Bool ← field json "addinQuiesced"
  let stateOwnedByRuntime : Bool ← field json "stateOwnedByRuntime"
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
    returnFreeOperations,
    asyncTasks,
    asyncExecutorRunning,
    rtdOperations,
    subscriptions,
    callbacks,
    rtdClassFactories,
    rtdServers,
    rtdServerLocks,
    handles,
    stateUnique,
    addinQuiesced,
    stateOwnedByRuntime,
    diagnosticsPending,
    diagnosticsRunning,
    cleanupIssues
  }

private def parseState (json : Json) : Except String WireState := do
  return {
    generation := (← field json "generation"),
    state := {
      phase := (← parsePhase (← json.getObjVal? "phase")),
      resources := (← parseResources (← json.getObjVal? "resources"))
    }
  }

private def parseTrace (json : Json) : Except String WireTrace := do
  let eventsJson : Array Json ← field json "events"
  return {
    schemaVersion := (← field json "schema_version")
    generation := (← field json "generation")
    initial := (← parseState (← json.getObjVal? "initial"))
    events := (← eventsJson.mapM parseEvent)
    traceTruncated := (← field json "trace_truncated")
    outcome := (← field json "outcome")
  }

private def checkEventWithProof (current : WireState) (event : Event) :
    Except String (Subtype fun next => wireRefinement.concreteStep current event next) := do
  match h : apply? current.state event with
  | none => throw "event is rejected by the formal transition"
  | some next =>
      let expected : WireState := { generation := current.generation, state := next }
      have hStep : wireRefinement.concreteStep current event expected := by
        change current.generation = expected.generation ∧
          apply? current.state event = some expected.state
        constructor
        · rfl
        · simpa [expected] using h
      return ⟨expected, hStep⟩

private def replayEvents :
    (events : List Event) →
    (current : WireState) →
    Except String (Subtype fun final => ConcreteSteps wireRefinement current events final)
  | [], current => return ⟨current, .refl current⟩
  | event :: rest, current => do
      let step ← checkEventWithProof current event
      let ⟨middle, hStep⟩ := step
      let result ← replayEvents rest middle
      let ⟨final, hRest⟩ := result
      return ⟨final, .cons hStep hRest⟩

private def checkReturnedSuccess
    (initial final : WireState)
    (events : List Event)
    (hInitialOpen : (wireRefinement.abstract initial).phase = .open)
    (hSteps : ConcreteSteps wireRefinement initial events final) : Except String Unit := by
  if h : wireReturnedSuccessBool final then
    let hSuccess := wire_returned_success_of_bool h
    let _quiescent := wire_refinement_success_is_quiescent hInitialOpen hSteps hSuccess
    exact .ok ()
  else
    exact .error "returned-success trace does not satisfy the concrete refinement"

private def checkTrace (json : Json) : Except String Unit := do
  let trace ← parseTrace json
  if trace.schemaVersion != schemaVersion then
    throw s!"unsupported shutdown trace schema version: {trace.schemaVersion}"
  if trace.generation == 0 then
    throw "shutdown trace generation must be non-zero"
  if trace.generation != trace.initial.generation then
    throw "shutdown trace generation does not match its initial state"
  if hInitial : trace.initial.state.phase == .open then
    let hInitialOpen := wire_initial_open_of_bool hInitial
    if trace.traceTruncated then
      throw "shutdown trace exceeded its in-memory event budget"
    let replayed ← replayEvents trace.events.toList trace.initial
    let ⟨final, hSteps⟩ := replayed
    match trace.outcome with
    | "in_progress" => return ()
    | "returned_success" =>
        checkReturnedSuccess trace.initial final trace.events.toList hInitialOpen hSteps
    | "fail_stopped" =>
        match final.state.phase with
        | .failStopped _ => return ()
        | _ => throw "fail-stopped trace does not end in a fail-stopped phase"
    | outcome => throw s!"unknown shutdown trace outcome: {outcome}"
  else
    throw "shutdown trace initial state must be open"

def main : IO UInt32 := do
  let stdin ← IO.getStdin
  let input ← stdin.readToEnd
  match Json.parse input >>= checkTrace with
  | .ok _ =>
      IO.println "valid shutdown trace"
      return 0
  | .error message =>
      IO.eprintln s!"invalid shutdown trace: {message}"
      return 1

end XlFnFormal.Shutdown

def main : IO UInt32 := XlFnFormal.Shutdown.main
