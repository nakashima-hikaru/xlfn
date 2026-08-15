import XlFnFormal.Handle.Registry.Snapshot.Safety

set_option autoImplicit false

namespace XlFnFormal.Handle.Registry.Snapshot

open XlFnFormal.Handle.Registry

/-! Small proof witnesses for the RCU protocol.  The runtime tests exercise
    the same races with real ArcSwap snapshots; these theorems keep the
    formal vocabulary focused on borrow and retirement rather than dynamic
    ownership. -/

theorem initial_state_has_no_borrows (session : SessionId) :
    (initialState session).borrows = [] := by
  rfl

theorem stale_publication_requires_retirement_after_borrow_release
    {s s' : State} {slot : SlotId} {generation : Generation}
    (hStep : Step s (.retirePublication slot generation) s') :
    s.findBorrowFor? slot generation = none :=
  retire_requires_no_borrow hStep

theorem close_does_not_retain_new_snapshot_reads
    {s s' : State} (hStep : Step s .closeRegistry s') :
    s'.snapshot = [] :=
  close_removes_new_borrow_source hStep

end XlFnFormal.Handle.Registry.Snapshot
