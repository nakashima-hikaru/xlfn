import XlFnFormal.Handle.Registry.Invariant

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Registry.Snapshot

open XlFnFormal.Handle.Registry

inductive PublicationState where
  | live
  | stale
  | closing
deriving DecidableEq, Repr

structure Publication where
  slot : SlotId
  generation : Generation
  state : PublicationState
deriving DecidableEq, Repr

structure SnapshotBinding where
  slot : SlotId
  generation : Generation
deriving DecidableEq, Repr

inductive FastLookupStage where
  | observed
  | tentative
  | validated
deriving DecidableEq, Repr

structure FastLookup where
  id : Nat
  token : Token
  stage : FastLookupStage
deriving DecidableEq, Repr

structure State where
  registry : Registry.State
  publications : List Publication
  snapshot : List SnapshotBinding
  fastLookups : List FastLookup
deriving DecidableEq, Repr

def initialState (session : SessionId) : State :=
  { registry := Registry.initialState session
    publications := []
    snapshot := []
    fastLookups := [] }

def State.findPublication?
    (s : State) (slot : SlotId) (generation : Generation) : Option Publication :=
  s.publications.find? (fun p => p.slot == slot && p.generation == generation)

def State.findSnapshot?
    (s : State) (slot : SlotId) : Option SnapshotBinding :=
  s.snapshot.find? (fun b => b.slot == slot)

def State.findFastLookup?
    (s : State) (id : Nat) : Option FastLookup :=
  s.fastLookups.find? (fun l => l.id == id)

def State.updatePublicationState
    (s : State) (slot : SlotId) (generation : Generation)
    (state : PublicationState) : List Publication :=
  s.publications.map (fun p =>
    if p.slot == slot && p.generation == generation then
      { p with state := state }
    else p)

def State.updateClosingPublications (s : State) : List Publication :=
  s.publications.map (fun p =>
    if p.state = .live then { p with state := .closing } else p)

def State.removeSnapshot (s : State) (slot : SlotId) : List SnapshotBinding :=
  s.snapshot.filter (fun b => b.slot != slot)

def State.removeFastLookup (s : State) (id : Nat) : List FastLookup :=
  s.fastLookups.filter (fun l => l.id != id)

def State.updateFastLookupStage
    (s : State) (id : Nat) (stage : FastLookupStage) : List FastLookup :=
  s.fastLookups.map (fun l => if l.id = id then { l with stage := stage } else l)

def State.observedFastLookups (s : State) : List FastLookup :=
  s.fastLookups.filter (fun l => l.stage = .observed)

def State.tentativeFastLookups (s : State) : List FastLookup :=
  s.fastLookups.filter (fun l => l.stage = .tentative)

def State.validatedFastLookups (s : State) : List FastLookup :=
  s.fastLookups.filter (fun l => l.stage = .validated)

def State.PublicationIdentitiesUnique (s : State) : Prop :=
  s.publications.Pairwise (fun lhs rhs => lhs.slot ≠ rhs.slot ∨ lhs.generation ≠ rhs.generation)

def State.SnapshotSlotsUnique (s : State) : Prop :=
  s.snapshot.Pairwise (fun lhs rhs => lhs.slot ≠ rhs.slot)

def State.FastLookupsUnique (s : State) : Prop :=
  s.fastLookups.Pairwise (fun lhs rhs => lhs.id ≠ rhs.id)

def State.LivePublicationSound (s : State) : Prop :=
  ∀ pub ∈ s.publications,
    pub.state = .live →
      ∃ h : pub.slot < s.registry.slots.length,
        s.registry.slots.get ⟨pub.slot, h⟩ = .live pub.generation

def State.LiveSnapshotSound (s : State) : Prop :=
  ∀ binding ∈ s.snapshot,
    ∃ pub ∈ s.publications,
      pub.slot = binding.slot ∧
      pub.generation = binding.generation ∧
      pub.state = .live

def State.LiveSnapshotRootIsLive (s : State) : Prop :=
  ∀ binding ∈ s.snapshot,
    ∃ h : binding.slot < s.registry.slots.length,
      s.registry.slots.get ⟨binding.slot, h⟩ = .live binding.generation

def State.FastLookupSound (s : State) : Prop :=
  ∀ lookup ∈ s.fastLookups,
    lookup.token.session = s.registry.session ∧
    ∃ pub ∈ s.publications,
      pub.slot = lookup.token.slot ∧
      pub.generation = lookup.token.generation

def State.LeaseAccounting (s : State) : Prop :=
  s.validatedFastLookups.length ≤ s.registry.activeLeases

def State.ClosedNoLiveSlots (s : State) : Prop :=
  s.registry.closed = true →
    Registry.NoLiveSlots s.registry ∧
    s.snapshot = [] ∧
    (∀ pub ∈ s.publications, pub.state ≠ .live)

def State.Invariant (s : State) : Prop :=
  s.PublicationIdentitiesUnique ∧
  s.SnapshotSlotsUnique ∧
  s.FastLookupsUnique ∧
  s.LivePublicationSound ∧
  s.LiveSnapshotSound ∧
  s.LiveSnapshotRootIsLive ∧
  s.FastLookupSound ∧
  s.LeaseAccounting ∧
  s.ClosedNoLiveSlots

theorem initialInvariant (session : SessionId) :
    (initialState session).Invariant := by
  refine ⟨List.Pairwise.nil, List.Pairwise.nil, List.Pairwise.nil,
    ?_, ?_, ?_, ?_, ?_, ?_⟩
  · intro pub hMem hLive
    contradiction
  · intro b hMem
    contradiction
  · intro b hMem
    contradiction
  · intro l hMem
    contradiction
  · exact Nat.le_refl 0
  · intro hClosed
    dsimp [initialState, Registry.initialState] at hClosed
    contradiction

end XlFnFormal.Handle.Registry.Snapshot
