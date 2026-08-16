import XlFnFormal.Handle.Topics.Serialization.Format

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Topics.Serialization

def splitFirst (bytes : ByteString) : Option (ByteString × ByteString) :=
  match bytes with
  | [] => none
  | byte :: rest =>
      if byte = separator then
        some ([], rest)
      else
        match splitFirst rest with
        | none => none
        | some (leftBytes, suffix) => some (byte :: leftBytes, suffix)

def splitThree (bytes : ByteString) :
    Option (ByteString × ByteString × ByteString × ByteString) :=
  match splitFirst bytes with
  | none => none
  | some (first, rest₁) =>
      match splitFirst rest₁ with
      | none => none
      | some (second, rest₂) =>
          match splitFirst rest₂ with
          | none => none
          | some (third, rest₃) => some (first, second, third, rest₃)

def splitDigestSuffix (bytes : ByteString) : Option (ByteString × ByteString) :=
  if hLength : 65 ≤ bytes.length then
    let udfLength := bytes.length - 65
    let udf := bytes.take udfLength
    let suffix := bytes.drop udfLength
    match suffix with
    | separator' :: digest =>
        if separator' = separator ∧ digest.length = 64 then
          some (udf, digest)
        else none
    | _ => none
  else none

def parseNatBytes (bytes : ByteString) : Option Nat :=
  (String.fromUTF8? bytes.toByteArray).bind String.toNat?

def parseIntBytes (bytes : ByteString) : Option Int :=
  (String.fromUTF8? bytes.toByteArray).bind String.toInt?

def hexValue (value : UInt8) : Option Nat :=
  let n := value.toNat
  if 48 ≤ n ∧ n ≤ 57 then some (n - 48)
  else if 97 ≤ n ∧ n ≤ 102 then some (n - 87)
  else none

def parseHexByte : ByteString → Option UInt8
  | [left, right] =>
      match hexValue left, hexValue right with
      | some high, some low => some (UInt8.ofNat (high * 16 + low))
      | _, _ => none
  | _ => none

def parseDigestPairs : Nat → ByteString → Option ByteString
  | 0, [] => some []
  | 0, _ => none
  | count + 1, left :: right :: rest =>
      match parseHexByte [left, right], parseDigestPairs count rest with
      | some byte, some parsed => some (byte :: parsed)
      | _, _ => none
  | _, _ => none

def vectorOfBytes32 (bytes : ByteString) (hLength : bytes.length = 32) :
    Vector UInt8 32 :=
  { toArray := bytes.toArray
    size_toArray := by simp [hLength] }

def parseDigest (bytes : ByteString) : Option (Vector UInt8 32) :=
  match parseDigestPairs 32 bytes with
  | none => none
  | some parsed =>
      if hLength : parsed.length = 32 then
        some (vectorOfBytes32 parsed hLength)
      else none

def parseRtdKey (bytes : ByteString) : Option FormulaRevisionKeyWire :=
  match splitThree bytes with
  | none => none
  | some (sheetBytes, rowBytes, columnBytes, udfAndDigest) =>
      match splitDigestSuffix udfAndDigest with
      | none => none
      | some (udfId, digestBytes) =>
          match parseNatBytes sheetBytes, parseIntBytes rowBytes,
              parseIntBytes columnBytes, parseDigest digestBytes with
          | some sheetId, some row, some column, some inputFingerprint =>
              some { sheetId, row, column, udfId, inputFingerprint }
          | _, _, _, _ => none

def parseCanonicalRtdKey (bytes : ByteString) : Option FormulaRevisionKeyWire :=
  match parseRtdKey bytes with
  | some key =>
      if formatRtdKey key = bytes then some key else none
  | none => none

def parseRtdKeyFor (width : PointerWidth) (bytes : ByteString) :
    Option FormulaRevisionKeyWire := by
  exact match parseCanonicalRtdKey bytes with
    | some key => if h : WellFormed width key then some key else none
    | none => none

end XlFnFormal.Handle.Topics.Serialization
