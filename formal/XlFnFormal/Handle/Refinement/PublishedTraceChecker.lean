import XlFnFormal.Handle.Refinement.PublishedChecker
import XlFnFormal.Handle.Topics.Serialization.Parser
import Lean.Data.Json

set_option autoImplicit false

namespace XlFnFormal.Handle.Refinement

open Lean
open XlFnFormal.Handle.Topics.Serialization

private def schemaVersion : Nat := 1

private def field {α : Type} [FromJson α] (json : Json) (name : String) :
    Except String α :=
  json.getObjValAs? α name

private def parseToken (json : Json) : Except String Registry.Token := do
  return {
    session := ← field json "session"
    slot := ← field json "slot"
    generation := ← field json "generation"
  }

private def parseTokenField (json : Json) : Except String Registry.Token := do
  parseToken (← json.getObjVal? "token")

private def parseOwner (json : Json) : Except String Topics.ExcelOwnerId := do
  return {
    serverGeneration := ← field json "serverGeneration"
    topicId := ← field json "topicId"
  }

private def parseOwnerField (json : Json) : Except String Topics.ExcelOwnerId := do
  parseOwner (← json.getObjVal? "owner")

private def digestNat (digest : Vector UInt8 32) : Nat :=
  digest.toArray.toList.foldl (fun accumulated byte => accumulated * 256 + byte.toNat) 0

private def parseTopicKeyWire
    (json : Json) : Except String (Topics.TopicKey × FormulaTopicKeyWire) := do
  let sheetId : Nat ← field json "sheetId"
  let row : Int ← field json "row"
  let column : Int ← field json "column"
  let udfId : String ← field json "udfId"
  let digestHex : String ← field json "argumentDigest"
  let digest ← match parseDigest (utf8Bytes digestHex) with
    | some value => pure value
    | none => throw "topic key digest must be 32 lower-case hexadecimal bytes"
  let wire : FormulaTopicKeyWire := {
    sheetId
    row
    column
    udfId := utf8Bytes udfId
    argumentDigest := digest
  }
  return ({
    sheetId
    row
    column
    udfId
    argumentDigest := digestNat digest
  }, wire)

private def parseTopicKeyField (json : Json) : Except String Topics.TopicKey := do
  let parsed ← parseTopicKeyWire (← json.getObjVal? "key")
  return parsed.1

private def parsePublishedTopicKey
    (json : Json) (rtdKey : String) : Except String Topics.TopicKey := do
  let (key, wire) ← parseTopicKeyWire json
  let encoded := parseCanonicalRtdKey (utf8Bytes rtdKey)
  if encoded != some wire then
    throw "publish event RTD key does not canonically encode its topic key"
  return key

private def parseEvent (json : Json) : Except String XlFnFormal.Handle.Refinement.Event := do
  let tag : String ← field json "event"
  match tag with
  | "beginPrepare" => return .topic .beginPrepare
  | "endPrepare" => return .topic .endPrepare
  | "beginInitializer" =>
      return .topic (.beginInitializer
        (← parseTopicKeyField json) (← field json "runtimeId"))
  | "finishInitializer" =>
      return .topic (.finishInitializer
        (← parseTopicKeyField json) (← field json "runtimeId"))
  | "insertPendingFresh" =>
      return .topic (.insertPendingFresh
        (← parseTopicKeyField json) (← field json "runtimeId"))
  | "insertPendingReuse" =>
      return .topic (.insertPendingReuse
        (← parseTopicKeyField json)
        (← field json "runtimeId")
        (← field json "slot")
        (← field json "generation"))
  | "publishAndInstallProvisional" =>
      let rtdKey : String ← field json "rtdKey"
      return .publishAndInstallProvisional
        (← parsePublishedTopicKey (← json.getObjVal? "key") rtdKey)
        (← field json "runtimeId")
        (← parseTokenField json)
        rtdKey
  | "commitAndActivate" =>
      return .commitAndActivate
        (← parseTopicKeyField json)
        (← field json "runtimeId")
        (← parseTokenField json)
  | "withdrawAndInvalidate" =>
      return .withdrawAndInvalidate
        (← parseTopicKeyField json)
        (← field json "runtimeId")
        (← parseTokenField json)
  | "rollbackPendingReuse" =>
      return .topic (.rollbackPendingReuse
        (← parseTopicKeyField json)
        (← field json "runtimeId")
        (← field json "nextGeneration"))
  | "rollbackPendingRetire" =>
      return .topic (.rollbackPendingRetire
        (← parseTopicKeyField json)
        (← field json "runtimeId"))
  | "beginWarmRead" =>
      return .beginWarmRead
        (← field json "readerId") (← parseTopicKeyField json)
  | "finishWarmRead" => return .finishWarmRead (← field json "readerId")
  | "failWarmRead" => return .failWarmRead (← field json "readerId")
  | "abandonWarmRead" => return .abandonWarmRead (← field json "readerId")
  | "claimServer" =>
      return .topic (.claimServer
        (← parseTopicKeyField json) (← field json "generation"))
  | "beginConnection" =>
      return .topic (.beginConnection
        (← parseTopicKeyField json) (← parseOwnerField json))
  | "reuseCommittedConnection" =>
      return .topic (.reuseCommittedConnection
        (← parseTopicKeyField json) (← parseOwnerField json))
  | "commitConnection" =>
      return .topic (.commitConnection
        (← parseTopicKeyField json) (← parseOwnerField json))
  | "rollbackConnection" =>
      return .topic (.rollbackConnection
        (← parseTopicKeyField json) (← parseOwnerField json))
  | "disconnect" =>
      return .disconnect
        (← parseTopicKeyField json) (← parseOwnerField json)
  | "detachGeneration" => return .detachGeneration (← field json "generation")
  | "drainPendingReuse" =>
      return .drainPendingReuse
        (← parseTokenField json)
        (← field json "runtimeId")
        (← field json "nextGeneration")
  | "drainPendingRetire" =>
      return .drainPendingRetire
        (← parseTokenField json) (← field json "runtimeId")
  | "drainPublishedReuse" =>
      return .drainPublishedReuse
        (← parseTokenField json) (← field json "nextGeneration")
  | "drainPublishedRetire" =>
      return .drainPublishedRetire (← parseTokenField json)
  | "sealForClose" => return .sealForClose
  | "closeRegistry" => return .closeRegistry
  | "finishClose" => return .topic .finishClose
  | tag => throw s!"unknown H4 handle refinement event: {tag}"

private structure WireTrace where
  schemaVersion : Nat
  initialSession : Nat
  events : Array XlFnFormal.Handle.Refinement.Event
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
    List XlFnFormal.Handle.Refinement.Event →
    XlFnFormal.Handle.Refinement.State →
    Except String XlFnFormal.Handle.Refinement.State
  | [], current => return current
  | event :: rest, current =>
      match apply? current event with
      | none => throw "H4 handle refinement event rejected"
      | some next => replayEvents rest next

private def closeCertified (state : XlFnFormal.Handle.Refinement.State) : Bool :=
  state.topics.runtime.phase == .closed &&
  state.topics.byKey == [] &&
  state.topics.byRtdKey == [] &&
  state.topics.byExcelOwner == [] &&
  state.topics.initializing == [] &&
  state.topics.detached == [] &&
  state.snapshot == [] &&
  state.warmReads == []

private def checkTrace (json : Json) : Except String Unit := do
  let trace ← parseTrace json
  if trace.schemaVersion != schemaVersion then
    throw s!"unsupported H4 handle refinement schema version: {trace.schemaVersion}"
  if trace.traceTruncated then
    throw "H4 handle refinement trace exceeded its in-memory event budget"
  let final ← replayEvents trace.events.toList
    (XlFnFormal.Handle.Refinement.initialState
      (Topics.initialState trace.initialSession))
  match trace.outcome with
  | "in_progress" => return ()
  | "returned_success" | "returnedSuccess" =>
      if closeCertified final then return ()
      throw "returned-success H4 handle trace did not reach CloseCertified"
  | outcome => throw s!"unknown H4 handle refinement outcome: {outcome}"

def main : IO UInt32 := do
  let stdin ← IO.getStdin
  let input ← stdin.readToEnd
  match Json.parse input >>= checkTrace with
  | .ok _ =>
      IO.println "valid H4 handle refinement trace"
      return 0
  | .error message =>
      IO.eprintln s!"invalid H4 handle refinement trace: {message}"
      return 1

end XlFnFormal.Handle.Refinement

def main : IO UInt32 := XlFnFormal.Handle.Refinement.main
