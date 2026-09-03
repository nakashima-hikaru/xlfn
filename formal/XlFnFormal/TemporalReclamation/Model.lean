/-! # Generic Temporal Reclamation Protocol

    Formalizes the protocol where an addressable object transitions through:
      Unpublished → Published → Retired → Reclaimed

    Guarded by:
      - `admissions`: short-lived lookup admission domain permits (e.g. `StripedDrainGate`)
      - `observing`: readers currently holding admission that have observed the raw pointer
        and are in the process of acquiring a pin
      - `pins`: long-lived capabilities (e.g. `CacheLease`, `HandleLease`, active tasks)

    Core safety invariant:
      `live capability => status ≠ .reclaimed`
-/

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.TemporalReclamation

inductive ObjectState where
  | unpublished
  | published
  | retired
  | reclaimed
deriving DecidableEq, Repr

structure State where
  status      : ObjectState
  admissions  : Nat
  observing   : Nat
  pins        : Nat
deriving DecidableEq, Repr

def initialState : State :=
  { status := .unpublished
  , admissions := 0
  , observing := 0
  , pins := 0 }

/-- The fundamental safety invariant of temporal reclamation. -/
def State.Invariant (s : State) : Prop :=
  -- TR-OBSERVE-1: Any observer implies object is published or retired (not reclaimed or unpublished).
  (s.observing > 0 → s.status = .published ∨ s.status = .retired) ∧
  -- TR-LEASE-1: Any active pin implies object is published or retired (not reclaimed or unpublished).
  (s.pins > 0 → s.status = .published ∨ s.status = .retired) ∧
  -- TR-ADMISSION-1: Pointer observation can only occur while an admission is held.
  (s.observing ≤ s.admissions) ∧
  -- TR-RECLAIM-1: Reclaimed status requires zero admissions, zero observers, and zero pins.
  (s.status = .reclaimed → s.admissions = 0 ∧ s.observing = 0 ∧ s.pins = 0) ∧
  -- TR-PUBLISH-1: Unpublished status requires zero observers and zero pins.
  (s.status = .unpublished → s.observing = 0 ∧ s.pins = 0)

end XlFnFormal.TemporalReclamation
