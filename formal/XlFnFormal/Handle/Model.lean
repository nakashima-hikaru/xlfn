import Std

set_option autoImplicit false

namespace XlFnFormal.Handle

abbrev SessionId := Nat
abbrev SlotId := Nat
abbrev Generation := Nat

def maxGeneration : Generation := 2 ^ 64 - 1

def nextGeneration? (g : Generation) : Option Generation :=
  if g < maxGeneration then some (g + 1) else none

theorem max_generation_has_no_successor :
    nextGeneration? maxGeneration = none := by
  simp [nextGeneration?, maxGeneration]

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
  activeInitializers : Nat
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

def MayInsert (s : State) : Prop :=
  s.phase = .«open» ∨ (s.phase = .drainingPrepares ∧ s.activeInitializers > 0)

instance (s : State) : Decidable s.MayInsert :=
  inferInstanceAs (Decidable (s.phase = .«open» ∨ (s.phase = .drainingPrepares ∧ s.activeInitializers > 0)))

def OperationInvariant (s : State) : Prop :=
  s.activeInitializers ≤ s.activePrepares

def CloseCertified (s : State) : Prop :=
  s.phase = .closed ∧
  s.activePrepares = 0 ∧
  s.activeInitializers = 0 ∧
  s.activeLeases = 0 ∧
  NoLiveSlots s.slots

def initialState (session : SessionId) : State :=
  { session := session
    phase := .«open»
    slots := []
    activePrepares := 0
    activeInitializers := 0
    activeLeases := 0 }

theorem initialState_noLiveSlots (session : SessionId) :
    NoLiveSlots (initialState session).slots := by
  intro slot hSlot
  simp [initialState] at hSlot

theorem initialState_operationInvariant (session : SessionId) :
    OperationInvariant (initialState session) := by
  dsimp [OperationInvariant, initialState]
  exact Nat.le_refl 0

end State

end XlFnFormal.Handle
