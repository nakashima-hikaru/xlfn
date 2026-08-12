import Std

set_option autoImplicit false

namespace XlFnFormal.Handle

abbrev SessionId := Nat
abbrev SlotId := Nat
abbrev Generation := Nat

def maxGeneration : Generation := 2 ^ 64 - 1

def nextGeneration? (g : Generation) : Option Generation :=
  if g < maxGeneration then some (g + 1) else none

inductive SlotState where
  | vacant (generation : Generation)
  | live (generation : Generation)
  | retired
  deriving DecidableEq, Repr

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

structure Token where
  session : SessionId
  slot : SlotId
  generation : Generation
  deriving DecidableEq, Repr

inductive Phase where
  | «open»
  | drainingPrepares
  | registryClosed
  | closed
  deriving DecidableEq, Repr

structure State where
  session : SessionId
  phase : Phase
  slots : List SlotState
  activePrepares : Nat
  activeLeases : Nat
  deriving DecidableEq, Repr

namespace State

def AuthenticatedFor (state : State) (token : Token) : Prop :=
  token.session = state.session

instance (state : State) (token : Token) : Decidable (AuthenticatedFor state token) :=
  inferInstanceAs (Decidable (token.session = state.session))

def SlotNoLive : SlotState → Prop
  | .vacant _ => True
  | .live _ => False
  | .retired => True

def NoLiveSlots (slots : List SlotState) : Prop :=
  ∀ slot ∈ slots, SlotNoLive slot

def CloseCertified (s : State) : Prop :=
  s.phase = .closed ∧
  s.activePrepares = 0 ∧
  s.activeLeases = 0 ∧
  NoLiveSlots s.slots

def initialState (session : SessionId) : State :=
  { session := session
    phase := .«open»
    slots := []
    activePrepares := 0
    activeLeases := 0 }

theorem initialState_noLiveSlots (session : SessionId) :
    NoLiveSlots (initialState session).slots := by
  intro slot hSlot
  simp [initialState] at hSlot

end State

end XlFnFormal.Handle
