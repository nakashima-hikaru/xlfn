import Init.Data.ByteArray.Lemmas
import Init.Data.String.Decode

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Topics.Serialization

abbrev ByteString := List UInt8

inductive PointerWidth where
  | w32
  | w64
deriving DecidableEq, Repr

def PointerWidth.sheetLimit : PointerWidth → Nat
  | .w32 => 2 ^ 32
  | .w64 => 2 ^ 64

def i32Min : Int := -2147483648

def i32Max : Int := 2147483647

def IsI32 (value : Int) : Prop :=
  i32Min ≤ value ∧ value ≤ i32Max

def IsUtf8 (bytes : ByteString) : Prop :=
  ByteArray.validateUTF8 bytes.toByteArray = true

structure FormulaRevisionKeyWire where
  sheetId : Nat
  row : Int
  column : Int
  udfId : ByteString
  inputFingerprint : Vector UInt8 32
deriving DecidableEq, Repr

def WellFormed (width : PointerWidth) (key : FormulaRevisionKeyWire) : Prop :=
  key.sheetId < width.sheetLimit ∧
  IsI32 key.row ∧
  IsI32 key.column ∧
  IsUtf8 key.udfId

instance (width : PointerWidth) (key : FormulaRevisionKeyWire) :
    Decidable (WellFormed width key) := by
  unfold WellFormed IsI32 IsUtf8
  infer_instance

end XlFnFormal.Handle.Topics.Serialization
