import XlFnFormal.Handle.Registry.Invariant

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Registry.Snapshot

open XlFnFormal.Handle.Registry

/-! The published-handle model is deliberately an RCU model. A snapshot owns
    the publication root, and a successful borrow keeps that root reachable
    until `releaseBorrow`. There is no tentative ownership or admission phase:
    the Rust lifetime of `Handle<'call, T>` is the admission protocol. -/

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

structure Borrow where
  id : Nat
  token : Token
deriving DecidableEq, Repr

structure State where
  registry : Registry.State
  publications : List Publication
  snapshot : List SnapshotBinding
  borrows : List Borrow
deriving DecidableEq, Repr

def initialState (session : SessionId) : State :=
  { registry := Registry.initialState session
    publications := []
    snapshot := []
    borrows := [] }

def State.findPublication?
    (s : State) (slot : SlotId) (generation : Generation) : Option Publication :=
  s.publications.find? (fun p => p.slot == slot && p.generation == generation)

def State.findSnapshot? (s : State) (slot : SlotId) : Option SnapshotBinding :=
  s.snapshot.find? (fun b => b.slot == slot)

def State.findBorrow? (s : State) (id : Nat) : Option Borrow :=
  s.borrows.find? (fun b => b.id == id)

def State.findBorrowFor?
    (s : State) (slot : SlotId) (generation : Generation) : Option Borrow :=
  s.borrows.find? (fun b => b.token.slot == slot && b.token.generation == generation)

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

def State.removePublication
    (s : State) (slot : SlotId) (generation : Generation) : List Publication :=
  s.publications.filter (fun p => p.slot != slot || p.generation != generation)

def State.PublicationIdentitiesUnique (s : State) : Prop :=
  s.publications.Pairwise (fun lhs rhs =>
    lhs.slot ≠ rhs.slot ∨ lhs.generation ≠ rhs.generation)

def State.SnapshotSlotsUnique (s : State) : Prop :=
  s.snapshot.Pairwise (fun lhs rhs => lhs.slot ≠ rhs.slot)

def State.BorrowsUnique (s : State) : Prop :=
  s.borrows.Pairwise (fun lhs rhs => lhs.id ≠ rhs.id)

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

def State.BorrowRooted (s : State) : Prop :=
  ∀ borrow ∈ s.borrows,
    ∃ pub ∈ s.publications,
      pub.slot = borrow.token.slot ∧
      pub.generation = borrow.token.generation

def State.ClosedNoLiveSlots (s : State) : Prop :=
  s.registry.closed = true →
    Registry.NoLiveSlots s.registry ∧
    s.snapshot = [] ∧
    (∀ pub ∈ s.publications, pub.state ≠ .live)

def State.Invariant (s : State) : Prop :=
  s.PublicationIdentitiesUnique ∧
  s.SnapshotSlotsUnique ∧
  s.BorrowsUnique ∧
  s.LivePublicationSound ∧
  s.LiveSnapshotSound ∧
  s.BorrowRooted ∧
  s.ClosedNoLiveSlots

theorem initialInvariant (session : SessionId) :
    (initialState session).Invariant := by
  refine ⟨List.Pairwise.nil, List.Pairwise.nil, List.Pairwise.nil,
    ?_, ?_, ?_, ?_⟩
  · intro pub hMem hLive
    contradiction
  · intro binding hMem
    contradiction
  · intro borrow hMem
    contradiction
  · intro hClosed
    dsimp [initialState, Registry.initialState] at hClosed
    contradiction

end XlFnFormal.Handle.Registry.Snapshot
