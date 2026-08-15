import XlFnFormal.Handle.Registry.Snapshot.Checker
import Lean.Data.Json

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Registry.Snapshot

open Lean
open XlFnFormal.Handle.Registry

private def schemaVersion : Nat := 1

private def field {α : Type} [FromJson α] (json : Json) (name : String) :
    Except String α :=
  json.getObjValAs? α name

private def parseToken (json : Json) : Except String Token := do
  return {
    session := ← field json "session"
    slot := ← field json "slot"
    generation := ← field json "generation"
  }

private def parseTokenField (json : Json) : Except String Token := do
  parseToken (← json.getObjVal? "token")

private def parseEvent (json : Json) : Except String Event := do
  let tag : String ← field json "event"
  match tag with
  | "insertFresh" => return .insertFresh
  | "insertReuse" =>
      return .insertReuse (← field json "slot") (← field json "generation")
  | "removeReuse" =>
      return .removeReuse (← parseTokenField json) (← field json "nextGeneration")
  | "removeRetire" =>
      return .removeRetire (← parseTokenField json)
  | "beginFastObservation" =>
      return .beginFastObservation (← field json "readerId") (← parseTokenField json)
  | "acquireTentativeLease" =>
      return .acquireTentativeLease (← field json "readerId")
  | "abandonObservation" =>
      return .abandonObservation (← field json "readerId")
  | "validateFastLookup" =>
      return .validateFastLookup (← field json "readerId")
  | "rejectTentativeFastLookup" =>
      return .rejectTentativeFastLookup (← field json "readerId")
  | "completeFastLookup" =>
      return .completeFastLookup (← field json "readerId")
  | "fallbackFastLookup" =>
      return .fallbackFastLookup (← field json "readerId")
  | "beginSlowLookup" =>
      return .beginSlowLookup (← parseTokenField json)
  | "endSlowLookup" => return .endSlowLookup
  | "beginSealLeaseAdmission" => return .beginSealLeaseAdmission
  | "finishSealLeaseAdmission" => return .finishSealLeaseAdmission
  | "closeRegistry" => return .closeRegistry
  | "finishClose" => return .finishClose
  | tag => throw s!"unknown Snapshot event: {tag}"

private structure WireTrace where
  schemaVersion : Nat
  initialSession : Nat
  events : Array Event
  traceTruncated : Bool
  outcome : String

private def parseTrace (json : Json) : Except String WireTrace := do
  let initial ← json.getObjVal? "initial"
  let eventsJson : Array Json ← field json "events"
  return {
    schemaVersion := ← field json "schema_version"
    initialSession := ← field initial "session"
    events := ← eventsJson.mapM parseEvent
    traceTruncated := ← field json "trace_truncated"
    outcome := ← field json "outcome"
  }

private def replayEvents :
    List Event → State → Except String State
  | [], current => return current
  | event :: rest, current =>
      match apply? current event with
      | none => throw s!"Snapshot event rejected: {repr event}"
      | some next => replayEvents rest next

private def closeCertified (state : State) : Bool :=
  state.registry.closed == true &&
  state.registry.activeLeases == 0 &&
  state.snapshot == [] &&
  state.tentativeFastLookups == [] &&
  state.validatedFastLookups == [] &&
  state.leaseAdmission == .sealed

private def checkTrace (json : Json) : Except String Unit := do
  let trace ← parseTrace json
  if trace.schemaVersion != schemaVersion then
    throw s!"unsupported Snapshot schema version: {trace.schemaVersion}"
  if trace.traceTruncated then
    throw "Snapshot trace exceeded its in-memory event budget"
  let final ← replayEvents trace.events.toList (initialState trace.initialSession)
  match trace.outcome with
  | "in_progress" => return ()
  | "returned_success" | "returnedSuccess" =>
      if closeCertified final then return ()
      throw "returned-success Snapshot trace did not reach CloseCertified"
  | outcome => throw s!"unknown Snapshot outcome: {outcome}"

def main : IO UInt32 := do
  let stdin ← IO.getStdin
  let input ← stdin.readToEnd
  match Json.parse input >>= checkTrace with
  | .ok _ =>
      IO.println "valid Snapshot refinement trace"
      return 0
  | .error message =>
      IO.eprintln s!"invalid Snapshot refinement trace: {message}"
      return 1

end XlFnFormal.Handle.Registry.Snapshot

def main : IO UInt32 := XlFnFormal.Handle.Registry.Snapshot.main
