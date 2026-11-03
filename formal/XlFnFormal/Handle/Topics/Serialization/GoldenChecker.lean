import XlFnFormal.Handle.Topics.Serialization.Safety
import Lean.Data.Json

set_option autoImplicit false

namespace XlFnFormal.Handle.Topics.Serialization

open Lean

private structure GoldenVector where
  name : String
  sheetId : Nat
  row : Int
  column : Int
  udfId : String
  digestHex : String
  rtdKey : String

private def field {α : Type} [FromJson α] (json : Json) (name : String) :
    Except String α :=
  json.getObjValAs? α name

private def parseVector (json : Json) : Except String GoldenVector := do
  return {
    name := ← field json "name"
    sheetId := ← field json "sheet_id"
    row := ← field json "row"
    column := ← field json "column"
    udfId := ← field json "udf_id"
    digestHex := ← field json "digest_hex"
    rtdKey := ← field json "rtd_key"
  }

private def parseDigestHex (value : String) : Except String (Vector UInt8 32) :=
  match parseDigest (utf8Bytes value) with
  | some digest => .ok digest
  | none => .error "golden digest is not 32 lower-case hexadecimal bytes"

private def checkVector (vector : GoldenVector) : Except String Unit := do
  let digest ← parseDigestHex vector.digestHex
  let key : FormulaRevisionKeyWire :=
    { sheetId := vector.sheetId
      row := vector.row
      column := vector.column
      udfId := utf8Bytes vector.udfId
      inputFingerprint := digest }
  let expected := utf8Bytes vector.rtdKey
  if formatRtdKey key != expected then
    throw s!"Rust/Lean golden formatter mismatch: {vector.name}"
  if parseCanonicalRtdKey expected != some key then
    throw s!"canonical parser rejected golden vector: {vector.name}"

private def checkFile (json : Json) : Except String Unit := do
  let version ← field json "schema_version"
  if version != 1 then
    throw s!"unsupported serialization golden schema version: {version}"
  let vectors : Array Json ← field json "vectors"
  for vector in vectors do
    checkVector (← parseVector vector)

def main : IO UInt32 := do
  let stdin ← IO.getStdin
  let input ← stdin.readToEnd
  match Json.parse input >>= checkFile with
  | .ok _ =>
      IO.println "valid RTD serialization golden vectors"
      return 0
  | .error message =>
      IO.eprintln s!"invalid RTD serialization golden vectors: {message}"
      return 1

end XlFnFormal.Handle.Topics.Serialization

def main : IO UInt32 := XlFnFormal.Handle.Topics.Serialization.main
