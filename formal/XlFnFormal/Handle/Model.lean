import Std

set_option autoImplicit false

namespace XlFnFormal.Handle

abbrev SessionId := Nat
abbrev SlotId := Nat
abbrev Generation := Nat
abbrev InitializerId := Nat

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

inductive InitializerStage where
  | beforeInsert
  | pending (token : Token)
  | resolved
  deriving DecidableEq, Repr

structure Initializer where
  id : InitializerId
  stage : InitializerStage
  deriving DecidableEq, Repr

structure State where
  session : SessionId
  phase : Phase
  slots : List SlotState
  activePrepares : Nat
  initializers : List Initializer
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
  s.initializers = [] ∧
  s.activeLeases = 0 ∧
  NoLiveSlots s.slots

def initialState (session : SessionId) : State :=
  { session := session
    phase := .«open»
    slots := []
    activePrepares := 0
    initializers := []
    activeLeases := 0 }

theorem initialState_noLiveSlots (session : SessionId) :
    NoLiveSlots (initialState session).slots := by
  intro slot hSlot
  simp [initialState] at hSlot

def findInitializer? (s : State) (id : InitializerId) : Option Initializer :=
  s.initializers.find? (fun init => init.id = id)

def updateInitializer (s : State) (id : InitializerId) (newStage : InitializerStage) : List Initializer :=
  s.initializers.map (fun init => if init.id = id then { init with stage := newStage } else init)

def removeInitializer (s : State) (id : InitializerId) : List Initializer :=
  s.initializers.filter (fun init => init.id ≠ id)

theorem updateInitializer_length (s : State) (id : InitializerId) (stage : InitializerStage) :
    (s.updateInitializer id stage).length = s.initializers.length := by
  simp [updateInitializer]

theorem removeInitializer_length_le (s : State) (id : InitializerId) :
    (s.removeInitializer id).length ≤ s.initializers.length := by
  simp [removeInitializer]
  exact List.length_filter_le _ _

end State

end XlFnFormal.Handle
