import XlFnFormal.Handle.Publication.Transition

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Publication

theorem publication_temporal_invariant
    {s : State} (hInv : s.Invariant) {pub : Publication}
    (hMem : pub ∈ s.publications) :
    (pub.published = true → pub.owned = true) ∧
    (pub.admitted > 0 → pub.owned = true) ∧
    (pub.owned = false → pub.published = false ∧ pub.admitted = 0) ∧
    (pub.gate = .sealed → pub.published = false) :=
  hInv.2.2.1 pub hMem

theorem object_capability_sound
    {s : State} (hInv : s.Invariant) {pub : Publication}
    (hMem : pub ∈ s.publications) (hOwned : pub.owned = true) :
    ∃ obj ∈ s.objects,
      obj.id = pub.objectId ∧ obj.present = true ∧ obj.bindings > 0 :=
  hInv.2.2.2.1 pub hMem hOwned

theorem object_pins_sound
    {s : State} (hInv : s.Invariant) {obj : ObjectState}
    (hMem : obj ∈ s.objects) (hPins : obj.pins > 0) :
    obj.present = true :=
  hInv.2.2.2.2.1 obj hMem hPins

theorem object_not_present_drained
    {s : State} (hInv : s.Invariant) {obj : ObjectState}
    (hMem : obj ∈ s.objects) (hNotPres : obj.present = false) :
    obj.bindings = 0 ∧ obj.pins = 0 :=
  hInv.2.2.2.2.2 obj hMem hNotPres

end XlFnFormal.Handle.Publication
