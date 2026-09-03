import XlFnFormal.TemporalOwnership.Safety

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.TemporalOwnership

/-! Thin refinement of `GenerationServiceSlot<C, R, E>` to `TemporalOwnership.State`.

    In Rust, readers first acquire a permit from `DrainGate` before loading the
    `AtomicPtr<R>`.  `seal()` seals the gate, stores a null pointer, waits for
    all active readers to drain (`readers == 0`), and only then transfers the
    unique `Box<R>` ownership. -/

inductive ServiceSlotPhase where
  | closed
  | cold
  | initializing
  | ready
  | sealing
  | initFaulted
  | teardownFaulted
deriving DecidableEq, Repr

structure ServiceSlotState where
  phase   : ServiceSlotPhase
  readers : Nat
  boxHeld : Bool
deriving DecidableEq, Repr

def toTemporalState (s : ServiceSlotState) : State :=
  { ownerPresent := s.boxHeld
  , published    := (s.phase == .ready)
  , gate         := if s.phase == .ready then .open else .sealed
  , readers      := s.readers }

theorem ready_refinement
    (s : ServiceSlotState)
    (hPhase : s.phase = .ready)
    (hBox : s.boxHeld = true) :
    let t := toTemporalState s
    t.ownerPresent = true ∧ t.published = true ∧ t.gate = .open := by
  intro t
  dsimp [t, toTemporalState]
  rw [hPhase]
  refine ⟨hBox, by decide, by decide⟩

theorem sealing_refinement
    (s : ServiceSlotState)
    (hPhase : s.phase = .sealing) :
    (toTemporalState s).gate = .sealed := by
  dsimp [toTemporalState]
  rw [hPhase]
  decide

theorem closed_refinement
    (s : ServiceSlotState)
    (hPhase : s.phase = .closed) :
    (toTemporalState s).published = false := by
  dsimp [toTemporalState]
  rw [hPhase]
  decide

/-- Operation representing the final ownership transfer in `ServiceSeal`.
    The slot-owned `Box<R>` can be transferred out only after the gate is sealing
    and all active readers have drained (`readers = 0`). -/
inductive ServiceSealTransfer : ServiceSlotState → ServiceSlotState → Prop where
  | transfer
      {s : ServiceSlotState}
      (hSealing : s.phase = .sealing)
      (hDrained : s.readers = 0)
      (hBox : s.boxHeld = true) :
      ServiceSealTransfer s { s with phase := .closed, boxHeld := false }

theorem service_seal_requires_drained
    {s s' : ServiceSlotState}
    (hTransfer : ServiceSealTransfer s s') :
    s.phase = .sealing ∧ s.readers = 0 ∧ s.boxHeld = true := by
  cases hTransfer with
  | transfer hSealing hDrained hBox =>
      exact ⟨hSealing, hDrained, hBox⟩

end XlFnFormal.TemporalOwnership
