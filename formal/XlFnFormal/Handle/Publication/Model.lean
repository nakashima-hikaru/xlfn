import XlFnFormal.Handle.Registry.Invariant
import XlFnFormal.TemporalOwnership.Model

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Publication

open XlFnFormal.Handle.Registry
open XlFnFormal.TemporalOwnership (GateState)

/-! Published-handle protocol with explicit admission/drain and ObjectArena capabilities.

    In the redesigned architecture, `BindingTable` uniquely owns `Box<BindingRecord>`.
    `PublishedBindings` exposes an `AtomicPtr<BindingRecord>`, and readers acquire
    an `OwnedOperationGuard` (admission permit) before loading the pointer.
    The record holds a non-owning capability to an `ObjectCell` in the runtime's
    `ObjectArena`.

    Retirement seals the gate, clears publication, and waits for all active
    readers to drain (`admitted = 0`) before retiring the object capability
    and reclaiming the record. -/

inductive PublicationState where
  | live
  | stale
  | closing
  | retired
deriving DecidableEq, Repr

structure Publication where
  slot       : SlotId
  generation : Generation
  state      : PublicationState
  gate       : GateState
  admitted   : Nat
  published  : Bool
  owned      : Bool
  objectId   : Nat
deriving DecidableEq, Repr

structure ObjectState where
  id       : Nat
  present  : Bool
  bindings : Nat
  pins     : Nat
  retired  : Bool
deriving DecidableEq, Repr

def objectCanReclaim (obj : ObjectState) : Prop :=
  obj.retired = true ∧ obj.bindings = 0 ∧ obj.pins = 0

structure State where
  registry     : Registry.State
  publications : List Publication
  objects      : List ObjectState
  nextObjectId : Nat
deriving DecidableEq, Repr

def initialState (session : SessionId) : State :=
  { registry     := Registry.initialState session
  , publications := []
  , objects      := []
  , nextObjectId := 1 }

def State.findPublication?
    (s : State) (slot : SlotId) (generation : Generation) : Option Publication :=
  s.publications.find? (fun p => p.slot == slot && p.generation == generation)

def State.findObject? (s : State) (id : Nat) : Option ObjectState :=
  s.objects.find? (fun obj => obj.id == id)

def State.updatePublication
    (s : State) (slot : SlotId) (generation : Generation)
    (f : Publication → Publication) : List Publication :=
  s.publications.map (fun p =>
    if p.slot == slot && p.generation == generation then f p else p)

def State.updateObject
    (s : State) (id : Nat) (f : ObjectState → ObjectState) : List ObjectState :=
  s.objects.map (fun obj => if obj.id == id then f obj else obj)

def State.PublicationIdentitiesUnique (s : State) : Prop :=
  s.publications.Pairwise (fun lhs rhs =>
    lhs.slot ≠ rhs.slot ∨ lhs.generation ≠ rhs.generation)

def State.ObjectIdsUnique (s : State) : Prop :=
  s.objects.Pairwise (fun lhs rhs => lhs.id ≠ rhs.id)

def State.TemporalInvariant (s : State) : Prop :=
  ∀ pub ∈ s.publications,
    (pub.published = true → pub.owned = true) ∧
    (pub.admitted > 0 → pub.owned = true) ∧
    (pub.owned = false → pub.published = false ∧ pub.admitted = 0) ∧
    (pub.gate = .sealed → pub.published = false)

def State.ObjectCapabilitySound (s : State) : Prop :=
  ∀ pub ∈ s.publications,
    pub.owned = true →
      ∃ obj ∈ s.objects,
        obj.id = pub.objectId ∧ obj.present = true ∧ obj.bindings > 0

def State.ObjectPinsSound (s : State) : Prop :=
  ∀ obj ∈ s.objects,
    obj.pins > 0 → obj.present = true

def State.ObjectNotPresentDrained (s : State) : Prop :=
  ∀ obj ∈ s.objects,
    obj.present = false → obj.bindings = 0 ∧ obj.pins = 0

def State.Invariant (s : State) : Prop :=
  s.PublicationIdentitiesUnique ∧
  s.ObjectIdsUnique ∧
  s.TemporalInvariant ∧
  s.ObjectCapabilitySound ∧
  s.ObjectPinsSound ∧
  s.ObjectNotPresentDrained

theorem initialInvariant (session : SessionId) :
    (initialState session).Invariant := by
  refine ⟨List.Pairwise.nil, List.Pairwise.nil, ?_, ?_, ?_, ?_⟩
  · intro pub hMem; contradiction
  · intro pub hMem; contradiction
  · intro obj hMem; contradiction
  · intro obj hMem; contradiction

end XlFnFormal.Handle.Publication
