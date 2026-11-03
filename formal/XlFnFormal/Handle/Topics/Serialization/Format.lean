import XlFnFormal.Handle.Topics.Serialization.Model
import Std.Data.String.ToInt
import Init.Data.String.Decode

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Topics.Serialization

def separator : UInt8 := 0x1f

def utf8Bytes (value : String) : ByteString :=
  value.toByteArray.data.toList

theorem utf8Bytes_toByteArray (value : String) :
    (utf8Bytes value).toByteArray = value.toByteArray := by
  apply ByteArray.ext
  simp [utf8Bytes]

theorem utf8Bytes_append (left right : String) :
    utf8Bytes (left ++ right) = utf8Bytes left ++ utf8Bytes right := by
  simp [utf8Bytes, String.toByteArray_append]

def decimalNatBytes (value : Nat) : ByteString :=
  (Nat.toDigits 10 value).map Char.toUInt8

def decimalIntBytes : Int → ByteString
  | .ofNat value => decimalNatBytes value
  | .negSucc value => 45 :: decimalNatBytes (value + 1)

theorem decimalNatBytes_toByteArray (value : Nat) :
    (decimalNatBytes value).toByteArray = value.repr.toByteArray := by
  rw [Nat.repr_eq_ofList_toDigits, String.toByteArray_ofList]
  have hDigits : ∀ digits : List Char,
      (∀ digit ∈ digits, digit.isDigit = true) →
        (digits.map Char.toUInt8).toByteArray = digits.utf8Encode := by
    intro digits hDigitSet
    induction digits with
    | nil => simp
    | cons digit rest ih =>
        have hDigit := hDigitSet digit (by simp)
        have hRest : ∀ c ∈ rest, c.isDigit = true := by
          intro c hc
          exact hDigitSet c (by simp [hc])
        have hUtf8 : String.utf8EncodeChar digit = [digit.toUInt8] := by
          apply String.utf8EncodeChar_eq_singleton
          rw [Char.utf8Size_eq_one_iff, UInt32.le_iff_toNat_le]
          have hRange := Char.isDigit_iff_toNat.mp hDigit
          simp at hRange
          exact Nat.le_trans hRange.2 (by decide)
        change ([digit.toUInt8] ++ (rest.map Char.toUInt8)).toByteArray = _
        rw [List.utf8Encode_cons, List.utf8Encode_singleton, hUtf8,
          List.toByteArray_append]
        simp [ih hRest]
  exact hDigits (Nat.toDigits 10 value)
    (fun digit hDigit => Nat.isDigit_of_mem_toDigits
      (b := 10) (n := value) (by decide) (by decide) hDigit)

theorem decimalIntBytes_toByteArray (value : Int) :
    (decimalIntBytes value).toByteArray = value.repr.toByteArray := by
  cases value with
  | ofNat value => exact decimalNatBytes_toByteArray value
  | negSucc value =>
      simp only [decimalIntBytes, Int.repr]
      change ([45] ++ decimalNatBytes (value + 1)).toByteArray = _
      rw [List.toByteArray_append, decimalNatBytes_toByteArray]
      rfl

theorem decimalNatBytes_no_separator (value : Nat) :
    separator ∉ decimalNatBytes value := by
  intro h
  rcases List.mem_map.mp h with ⟨digit, hDigitMem, hEq⟩
  have hDigit := Nat.isDigit_of_mem_toDigits (b := 10) (n := value)
    (by decide) (by decide) hDigitMem
  have hRange := Char.isDigit_iff_toNat.mp hDigit
  simp at hRange
  have hEqNat := congrArg UInt8.toNat hEq
  simp only [Char.toUInt8, UInt32.toNat_toUInt8] at hEqNat
  simp [separator] at hEqNat
  omega

theorem decimalIntBytes_no_separator (value : Int) :
    separator ∉ decimalIntBytes value := by
  cases value with
  | ofNat value => exact decimalNatBytes_no_separator value
  | negSucc value =>
      intro h
      have hNe : (separator : UInt8) ≠ 45 := by decide
      apply hNe
      simpa [decimalIntBytes, decimalNatBytes_no_separator (value + 1)] using h

def hexNibble (value : Nat) : UInt8 :=
  UInt8.ofNat (if value < 10 then 48 + value else 87 + value)

def hexByte (value : UInt8) : ByteString :=
  [hexNibble (value.toNat / 16), hexNibble (value.toNat % 16)]

def formatDigest (digest : Vector UInt8 32) : ByteString :=
  digest.toList.flatMap hexByte

def formatRtdKey (key : FormulaRevisionKeyWire) : ByteString :=
  decimalNatBytes key.sheetId ++
    [separator] ++
    decimalIntBytes key.row ++
    [separator] ++
    decimalIntBytes key.column ++
    [separator] ++
    key.udfId ++
    [separator] ++
    formatDigest key.inputFingerprint

end XlFnFormal.Handle.Topics.Serialization
