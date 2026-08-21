import XlFnFormal.Handle.Registry.Model

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Registry.Object

open XlFnFormal.Handle.Registry

abbrev ObjectId := Nat
abbrev Epoch := Nat

structure ObjectKey where
  slot : SlotId
  generation : Generation
deriving DecidableEq, Repr

structure LiveObject where
  objectId : ObjectId
  key : ObjectKey
  pins : Nat
deriving DecidableEq, Repr

structure RetiredObject where
  objectId : ObjectId
  key : ObjectKey
  retireEpoch : Epoch
  pins : Nat
deriving DecidableEq, Repr

structure State where
  live : List LiveObject
  retired : List RetiredObject
  activeEpochs : List Epoch
deriving DecidableEq, Repr

def initialState : State :=
  { live := []
    retired := []
    activeEpochs := [] }

def PayloadExists (s : State) (objectId : ObjectId) (key : ObjectKey) : Prop :=
  (∃ object ∈ s.live, object.objectId = objectId ∧ object.key = key) ∨
  (∃ object ∈ s.retired, object.objectId = objectId ∧ object.key = key)

def Borrowed (s : State) (epoch : Epoch) (objectId : ObjectId) (key : ObjectKey) : Prop :=
  epoch ∈ s.activeEpochs ∧ PayloadExists s objectId key

def PinHeld (s : State) (objectId : ObjectId) (key : ObjectKey) : Prop :=
  (∃ object ∈ s.live, object.objectId = objectId ∧ object.key = key ∧ object.pins > 0) ∨
  (∃ object ∈ s.retired, object.objectId = objectId ∧ object.key = key ∧ object.pins > 0)

def Reclaimable (s : State) (object : RetiredObject) : Prop :=
  object ∈ s.retired ∧ object.pins = 0 ∧
    ∀ active ∈ s.activeEpochs, object.retireEpoch < active

def CanResurrect (s : State) (object : RetiredObject) : Prop :=
  object ∈ s.retired ∧ object.pins = 0

def resurrect (s : State) (object : RetiredObject) (newKey : ObjectKey) : State :=
  { s with
    live := { objectId := object.objectId, key := newKey, pins := 0 } :: s.live
    retired := s.retired.filter (fun candidate => candidate ≠ object) }

theorem borrowed_implies_payload_exists
    {s : State} {epoch : Epoch} {objectId : ObjectId} {key : ObjectKey}
    (hBorrowed : Borrowed s epoch objectId key) :
    PayloadExists s objectId key :=
  hBorrowed.2

theorem reclaimable_requires_all_active_epochs_after_retirement
    {s : State} {object : RetiredObject}
    (hReclaimable : Reclaimable s object) :
    ∀ active ∈ s.activeEpochs, object.retireEpoch < active :=
  hReclaimable.2.2

theorem resurrect_preserves_object_identity
    {s : State} {object : RetiredObject} {newKey : ObjectKey}
    (hRetired : object ∈ s.retired) :
    PayloadExists (resurrect s object newKey) object.objectId newKey := by
  left
  refine ⟨{ objectId := object.objectId, key := newKey, pins := 0 }, ?_, rfl, rfl⟩
  simp [resurrect]

theorem resurrect_removes_old_queue_entry
    {s : State} {object : RetiredObject} {newKey : ObjectKey}
    (hRetired : object ∈ s.retired) :
    object ∉ (resurrect s object newKey).retired := by
  simp [resurrect]

theorem reclaimable_object_cannot_have_a_pre_retirement_borrow
    {s : State} {object : RetiredObject} {epoch : Epoch}
    (hReclaimable : Reclaimable s object)
    (hBorrowed : Borrowed s epoch object.objectId object.key)
    (hBorrowEpoch : epoch ≤ object.retireEpoch) :
    False := by
  have hAfter := hReclaimable.2.2 epoch hBorrowed.1
  exact (Nat.not_lt_of_ge hBorrowEpoch) hAfter

end XlFnFormal.Handle.Registry.Object
