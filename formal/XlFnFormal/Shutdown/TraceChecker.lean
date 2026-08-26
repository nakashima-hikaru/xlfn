import XlFnFormal.Shutdown.Checker
import XlFnFormal.Shutdown.Refinement
import Lean.Data.Json

set_option autoImplicit false

namespace XlFnFormal.Shutdown

open Lean

private structure WireState where
  generation : Nat
  state : State
  deriving DecidableEq

/-! Activity observations are deliberately kept separate from the ordered
    certificate stream.  Activity observations describe ownership edges; the
    order in which concurrent observers appended them is not a protocol
    linearization order. -/
private inductive Activity where
  | enterExternal (id : Nat)
  | leaveExternal (id : Nat)
  | enterCall (id : Nat)
  | leaveCall (id : Nat)
  | createReturnBlock
  | beginReturnFree
  | releaseReturnBlock
  | endReturnFree
  | startAsyncTask
  | endAsyncTask (completion : Completion)
  | beginRtdOperation
  | endRtdOperation
  | addSubscription
  | removeSubscription
  | beginCallback
  | endCallback
  | addRtdClassFactory
  | removeRtdClassFactory
  | addRtdServer
  | removeRtdServer
  | lockRtdServer
  | unlockRtdServer
  | addHandle
  | removeHandle
  | addHandleObject
  | removeHandleObject
  | addHandlePin
  | removeHandlePin
  | enqueueDiagnostic
  | flushDiagnostic
  | discardDiagnostic
  | recordCleanupIssue

private structure WireTrace where
  generation : Nat
  initial : Resources
  activities : Array Activity
  certificates : Array Event
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

private def parseFailure : Json → Except String Failure
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

private def parseCompletion : Json → Except String Completion
  | .str "completed" => return .completed
  | .str "canceled" => return .canceled
  | .str "failed" => return .failed
  | json => throw s!"unknown async completion: {json}"

private def simpleCertificateEvent : String → Option Event
  | "registerFunction" => some .registerFunction
  | "unregisterFunction" => some .unregisterFunction
  | "registerEvent" => some .registerEvent
  | "unregisterEvent" => some .unregisterEvent
  | "startAsyncExecutor" => some .startAsyncExecutor
  | "stopAsyncExecutor" => some .stopAsyncExecutor
  | "startDiagnostics" => some .startDiagnostics
  | "stopDiagnostics" => some .stopDiagnostics
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

private def simpleActivityEvent : String → Option Activity
  | "createReturnBlock" => some .createReturnBlock
  | "beginReturnFree" => some .beginReturnFree
  | "releaseReturnBlock" => some .releaseReturnBlock
  | "endReturnFree" => some .endReturnFree
  | "startAsyncTask" => some .startAsyncTask
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
  | "addHandleObject" => some .addHandleObject
  | "removeHandleObject" => some .removeHandleObject
  | "addHandlePin" => some .addHandlePin
  | "removeHandlePin" => some .removeHandlePin
  | "enqueueDiagnostic" => some .enqueueDiagnostic
  | "flushDiagnostic" => some .flushDiagnostic
  | "discardDiagnostic" => some .discardDiagnostic
  | "recordCleanupIssue" => some .recordCleanupIssue
  | _ => none

private def parseCertificateEvent : Json → Except String Event
  | json =>
      match json.getTag? with
      | some "quarantine" => do
          let payload ← json.getObjVal? "quarantine"
          return .quarantine (← parseFailure payload)
      | some "failStop" => do
          let payload ← json.getObjVal? "failStop"
          return .failStop (← parseFailure payload)
      | some tag =>
          match simpleCertificateEvent tag with
          | some event => return event
          | none => throw s!"unknown shutdown event: {tag}"
      | none => throw s!"shutdown event must be a string or tagged object: {json}"

private def parseActivityEvent : Json → Except String Activity
  | json =>
      match json.getTag? with
      | some "enterExternal" => do
          let payload ← json.getObjVal? "enterExternal"
          return .enterExternal (← field payload "id")
      | some "leaveExternal" => do
          let payload ← json.getObjVal? "leaveExternal"
          return .leaveExternal (← field payload "id")
      | some "enterCall" => do
          let payload ← json.getObjVal? "enterCall"
          return .enterCall (← field payload "id")
      | some "leaveCall" => do
          let payload ← json.getObjVal? "leaveCall"
          return .leaveCall (← field payload "id")
      | some "endAsyncTask" => do
          let payload ← json.getObjVal? "endAsyncTask"
          return .endAsyncTask (← parseCompletion payload)
      | some tag =>
          match simpleActivityEvent tag with
          | some event => return event
          | none => throw s!"unknown shutdown activity: {tag}"
      | none => throw s!"shutdown activity must be a string or tagged object: {json}"

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
  let handlePins : Nat ← field json "handlePins"
  let handleObjects : Nat ← field json "handleObjects"
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
    handlePins,
    handleObjects,
    generationUnique,
    addinQuiesced,
    generationOwnedByRuntime,
    diagnosticsPending,
    diagnosticsRunning,
    cleanupIssues
  }

private def parseTrace (json : Json) : Except String WireTrace := do
  let activitiesJson : Array Json ← field json "activities"
  let certificatesJson : Array Json ← field json "certificates"
  return {
    generation := (← field json "generation")
    initial := (← parseResources (← json.getObjVal? "initial"))
    activities := (← activitiesJson.mapM parseActivityEvent)
    certificates := (← certificatesJson.mapM parseCertificateEvent)
    traceTruncated := (← field json "trace_truncated")
    outcome := (← field json "outcome")
  }

private def removeId (id : Nat) : List Nat → Option (List Nat)
  | [] => none
  | head :: tail =>
      if id = head then
        some tail
      else
        match removeId id tail with
        | some remaining => some (head :: remaining)
        | none => none

private def uniqueIds : List Nat → Bool
  | [] => true
  | head :: tail =>
      !tail.contains head && uniqueIds tail

private def idsFor : (Activity → Option Nat) → Array Activity → List Nat
  | selector, activities => activities.toList.filterMap selector

private def checkActivityPair (label : String) (enters leaves : List Nat) :
    Except String Unit := do
  if !uniqueIds enters then
    throw s!"duplicate {label} activity identifier"
  if !uniqueIds leaves then
    throw s!"duplicate {label} release identifier"
  for id in leaves do
    if id ∉ enters then
      throw s!"{label} release has no matching activity"

private def countActivity (predicate : Activity → Bool) : List Activity → Nat
  | [] => 0
  | head :: tail =>
      (if predicate head then 1 else 0) + countActivity predicate tail

private def checkCountPair (label : String) (starts stops : Nat) : Except String Unit := do
  if stops > starts then
    throw s!"{label} activity balance underflow"

private def checkActivities (activities : Array Activity) : Except String Unit := do
  let externalEnters := idsFor (fun activity =>
    match activity with
    | .enterExternal id => some id
    | _ => none) activities
  let externalLeaves := idsFor (fun activity =>
    match activity with
    | .leaveExternal id => some id
    | _ => none) activities
  let callEnters := idsFor (fun activity =>
    match activity with
    | .enterCall id => some id
    | _ => none) activities
  let callLeaves := idsFor (fun activity =>
    match activity with
    | .leaveCall id => some id
    | _ => none) activities
  checkActivityPair "external entry" externalEnters externalLeaves
  checkActivityPair "call" callEnters callLeaves
  let observations := activities.toList
  checkCountPair "return block" 
    (countActivity (fun activity => match activity with
      | .createReturnBlock => true
      | _ => false) observations)
    (countActivity (fun activity => match activity with
      | .releaseReturnBlock => true
      | _ => false) observations)
  checkCountPair "return free" 
    (countActivity (fun activity => match activity with
      | .beginReturnFree => true
      | _ => false) observations)
    (countActivity (fun activity => match activity with
      | .endReturnFree => true
      | _ => false) observations)
  checkCountPair "async task" 
    (countActivity (fun activity => match activity with
      | .startAsyncTask => true
      | _ => false) observations)
    (countActivity (fun activity => match activity with
      | .endAsyncTask _ => true
      | _ => false) observations)
  checkCountPair "RTD operation" 
    (countActivity (fun activity => match activity with
      | .beginRtdOperation => true
      | _ => false) observations)
    (countActivity (fun activity => match activity with
      | .endRtdOperation => true
      | _ => false) observations)
  checkCountPair "subscription" 
    (countActivity (fun activity => match activity with
      | .addSubscription => true
      | _ => false) observations)
    (countActivity (fun activity => match activity with
      | .removeSubscription => true
      | _ => false) observations)
  checkCountPair "callback" 
    (countActivity (fun activity => match activity with
      | .beginCallback => true
      | _ => false) observations)
    (countActivity (fun activity => match activity with
      | .endCallback => true
      | _ => false) observations)
  checkCountPair "RTD class factory" 
    (countActivity (fun activity => match activity with
      | .addRtdClassFactory => true
      | _ => false) observations)
    (countActivity (fun activity => match activity with
      | .removeRtdClassFactory => true
      | _ => false) observations)
  checkCountPair "RTD server" 
    (countActivity (fun activity => match activity with
      | .addRtdServer => true
      | _ => false) observations)
    (countActivity (fun activity => match activity with
      | .removeRtdServer => true
      | _ => false) observations)
  checkCountPair "RTD server lock" 
    (countActivity (fun activity => match activity with
      | .lockRtdServer => true
      | _ => false) observations)
    (countActivity (fun activity => match activity with
      | .unlockRtdServer => true
      | _ => false) observations)
  checkCountPair "handle" 
    (countActivity (fun activity => match activity with
      | .addHandle => true
      | _ => false) observations)
    (countActivity (fun activity => match activity with
      | .removeHandle => true
      | _ => false) observations)
  checkCountPair "handle object" 
    (countActivity (fun activity => match activity with
      | .addHandleObject => true
      | _ => false) observations)
    (countActivity (fun activity => match activity with
      | .removeHandleObject => true
      | _ => false) observations)
  checkCountPair "handle pin" 
    (countActivity (fun activity => match activity with
      | .addHandlePin => true
      | _ => false) observations)
    (countActivity (fun activity => match activity with
      | .removeHandlePin => true
      | _ => false) observations)

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
  if trace.generation == 0 then
    throw "shutdown trace generation must be non-zero"
  checkActivities trace.activities
  let initial : WireState := {
    generation := trace.generation
    state := State.opened trace.initial
  }
  if hInitial : initial.state.phase == .open then
    let hInitialOpen := wire_initial_open_of_bool hInitial
    if trace.traceTruncated then
      throw "shutdown trace exceeded its in-memory event budget"
    let replayed ← replayEvents trace.certificates.toList initial
    let ⟨final, hSteps⟩ := replayed
    match trace.outcome with
    | "in_progress" => return ()
    | "returned_success" =>
        checkReturnedSuccess initial final trace.certificates.toList hInitialOpen hSteps
    | "fail_stopped" =>
        match final.state.phase with
        | .failStopped _ => return ()
        | _ => throw "fail-stopped trace does not end in a fail-stopped phase"
    | "quarantined" =>
        match final.state.phase with
        | .quarantined _ => return ()
        | _ => throw "quarantine trace does not end in a quarantined phase"
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
