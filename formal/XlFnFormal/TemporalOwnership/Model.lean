/-! Generic temporal ownership protocol modeling `DrainGate + AtomicPtr + Box`.

    Tracks the temporal protocol where a single unique owner publishes an
    addressable pointer guarded by an admission gate, while readers acquire
    counted drain permits before loading.  Sealing the gate and withdrawing
    publication precedes reader drain, after which the unique owner can be
    safely reclaimed. -/

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.TemporalOwnership

inductive GateState where
  | open
  | sealed
deriving DecidableEq, Repr

structure State where
  ownerPresent : Bool
  published    : Bool
  gate         : GateState
  readers      : Nat
deriving DecidableEq, Repr

def initialState : State :=
  { ownerPresent := true
  , published := false
  , gate := .open
  , readers := 0 }

def State.Invariant (s : State) : Prop :=
  (s.published = true → s.ownerPresent = true) ∧
  (s.readers > 0 → s.ownerPresent = true) ∧
  (s.ownerPresent = false → s.published = false ∧ s.readers = 0)

end XlFnFormal.TemporalOwnership
