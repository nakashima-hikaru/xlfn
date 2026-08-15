import XlFnFormal.Handle.Registry.Snapshot.Transition

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Registry.Snapshot

open XlFnFormal.Handle.Registry

theorem borrow_rooted
    {s : State} (hInv : s.Invariant) {borrow : Borrow}
    (hMem : borrow ∈ s.borrows) :
    ∃ pub ∈ s.publications,
      pub.slot = borrow.token.slot ∧
      pub.generation = borrow.token.generation :=
  hInv.2.2.2.2.2.1 borrow hMem

theorem live_snapshot_has_live_publication
    {s : State} (hInv : s.Invariant) {binding : SnapshotBinding}
    (hMem : binding ∈ s.snapshot) :
    ∃ pub ∈ s.publications,
      pub.slot = binding.slot ∧
      pub.generation = binding.generation ∧
      pub.state = .live :=
  hInv.2.2.2.2.1 binding hMem

end XlFnFormal.Handle.Registry.Snapshot
