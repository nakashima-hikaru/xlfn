import XlFnFormal.Handle.Publication.Invariant
import XlFnFormal.Handle.Registry.Safety

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Publication

open XlFnFormal.Handle.Registry

/-- 1. Any active reader guarantees that the publication record is owned. -/
theorem readerImpliesOwned
    {s : State} (hInv : s.Invariant) {pub : Publication}
    (hMem : pub ∈ s.publications) (hAdm : pub.admitted > 0) :
    pub.owned = true :=
  (publication_temporal_invariant hInv hMem).2.1 hAdm

/-- 2. Once reclaimed (no longer owned), no readers can be active. -/
theorem reclaimedImpliesNoReaders
    {s : State} (hInv : s.Invariant) {pub : Publication}
    (hMem : pub ∈ s.publications) (hNotOwned : pub.owned = false) :
    pub.admitted = 0 :=
  (publication_temporal_invariant hInv hMem).2.2.1 hNotOwned |>.2

/-- 3. A sealed gate rejects admission of new readers. -/
theorem sealedImpliesNoNewReaders
    {s : State} {slot : SlotId} {generation : Generation} {pub : Publication}
    (hPub : s.findPublication? slot generation = some pub)
    (hSealed : pub.gate = .sealed) :
    ¬ ∃ s', Step s (.enterReader slot generation) s' := by
  intro ⟨s', hStep⟩
  cases hStep with
  | enterReader hPubStep hGate hPubVis hOwned =>
      rw [hPub] at hPubStep
      injection hPubStep with hEq
      subst hEq
      rw [hSealed] at hGate
      contradiction

/-- 4. Reclaiming a capability requires an unpublished and fully drained record. -/
theorem reclaimRequiresUnpublishedAndDrained
    {s s' : State} {slot : SlotId} {generation : Generation}
    (hStep : Step s (.retireCapability slot generation) s') :
    ∃ pub, s.findPublication? slot generation = some pub ∧
      pub.published = false ∧ pub.gate = .sealed ∧ pub.admitted = 0 ∧ pub.owned = true := by
  cases hStep with
  | retireCapability hPub hNotPub hSealed hDrained hOwned =>
      exact ⟨_, hPub, hNotPub, hSealed, hDrained, hOwned⟩

/-- An object with active readers via any publication cannot be reclaimed. -/
theorem borrowedObjectNotReclaimed
    {s : State} (hInv : s.Invariant) {pub : Publication}
    (hMem : pub ∈ s.publications) (hAdm : pub.admitted > 0) :
    ∃ obj ∈ s.objects, obj.id = pub.objectId ∧ obj.present = true := by
  have hOwned := readerImpliesOwned hInv hMem hAdm
  have ⟨obj, hObjMem, hId, hPres, hBindings⟩ := object_capability_sound hInv hMem hOwned
  exact ⟨obj, hObjMem, hId, hPres⟩

/-- An object held by a long-lived pin (HandleLease) cannot be reclaimed. -/
theorem pinnedObjectNotReclaimed
    {s : State} (hInv : s.Invariant) {obj : ObjectState}
    (hMem : obj ∈ s.objects) (hPins : obj.pins > 0) :
    obj.present = true :=
  object_pins_sound hInv hMem hPins

/-- Reclaiming an ObjectCell requires all capabilities (bindings and pins) to be zero. -/
theorem reclaimRequiresNoCapabilities
    {s s' : State} {objectId : Nat}
    (hStep : Step s (.reclaimObject objectId) s') :
    ∃ obj, s.findObject? objectId = some obj ∧
      obj.retired = true ∧ obj.bindings = 0 ∧ obj.pins = 0 := by
  cases hStep with
  | reclaimObject hObj hCan hPres =>
      exact ⟨_, hObj, hCan.1, hCan.2.1, hCan.2.2⟩

end XlFnFormal.Handle.Publication
