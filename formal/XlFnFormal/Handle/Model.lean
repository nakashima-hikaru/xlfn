import Std

set_option autoImplicit false

namespace XlFnFormal.Handle

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

def initialState (session : SessionId) (numSlots : Nat) : State :=
  { session := session
    phase := .«open»
    slots := List.replicate numSlots (.vacant 0)
    activePrepares := 0
    activeLeases := 0 }

theorem initialState_noLiveSlots (session : SessionId) (numSlots : Nat) :
    NoLiveSlots (initialState session numSlots).slots := by
  intro slot hSlot
  simp [initialState] at hSlot
  rw [hSlot.2]
  exact trivial

end State

end XlFnFormal.Handle
