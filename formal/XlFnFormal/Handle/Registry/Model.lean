set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Registry

abbrev SessionId := Nat
abbrev SlotId := Nat
abbrev Generation := Nat

structure Token where
  session : SessionId
  slot : SlotId
  generation : Generation
deriving DecidableEq, Repr

inductive SlotState where
  | vacant (generation : Generation)
  | live (generation : Generation)
  | retired
deriving DecidableEq, Repr

def maxGeneration : Generation := 2 ^ 64 - 1

def nextGeneration? (g : Generation) : Option Generation :=
  if g < maxGeneration then some (g + 1) else none

def closeSlot : SlotState → SlotState
  | .vacant g =>
      match nextGeneration? g with
      | some next => .vacant next
      | none => .retired
  | .live g =>
      match nextGeneration? g with
      | some next => .vacant next
      | none => .retired
  | .retired => .retired

structure State where
  session : SessionId
  slots : List SlotState
  activeLeases : Nat
  closed : Bool
deriving DecidableEq, Repr

def initialState (session : SessionId) : State :=
  { session := session
    slots := []
    activeLeases := 0
    closed := false }

def State.AuthenticatedFor (s : State) (token : Token) : Prop :=
  token.session = s.session

def SlotState.IsRetired : SlotState → Prop
  | .retired => True
  | _ => False

def SlotState.IsLive : SlotState → Prop
  | .live _ => True
  | _ => False

def NoLiveSlots (s : State) : Prop :=
  ∀ slot (h : slot < s.slots.length),
    ¬ (s.slots.get ⟨slot, h⟩).IsLive

def TokenLive (s : State) (token : Token) : Prop :=
  token.session = s.session ∧
  ∃ h : token.slot < s.slots.length,
    s.slots.get ⟨token.slot, h⟩ = .live token.generation

def State.MayInsert (s : State) : Prop :=
  s.closed = false

end XlFnFormal.Handle.Registry
