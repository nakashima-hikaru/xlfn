import XlFnFormal.Handle.Registry.Snapshot.Invariant
import XlFnFormal.Handle.Registry.Safety

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Registry.Snapshot

open XlFnFormal.Handle.Registry

theorem stale_publication_cannot_be_borrowed
    {s : State} {readerId : Nat} {token : Token} {pub : Publication}
    (hPub : s.findPublication? token.slot token.generation = some pub)
    (hStale : pub.state = .stale) :
    ¬ ∃ s', Step s (.observeBorrow readerId token) s' := by
  intro ⟨s', hStep⟩
  cases hStep with
  | observeBorrow _ _ _ hPubStep _ hLive _ =>
      rw [hPub] at hPubStep
      injection hPubStep with hEq
      subst hEq
      rw [hStale] at hLive
      contradiction

theorem closing_publication_cannot_be_borrowed
    {s : State} {readerId : Nat} {token : Token} {pub : Publication}
    (hPub : s.findPublication? token.slot token.generation = some pub)
    (hClosing : pub.state = .closing) :
    ¬ ∃ s', Step s (.observeBorrow readerId token) s' := by
  intro ⟨s', hStep⟩
  cases hStep with
  | observeBorrow _ _ _ hPubStep _ hLive _ =>
      rw [hPub] at hPubStep
      injection hPubStep with hEq
      subst hEq
      rw [hClosing] at hLive
      contradiction

theorem close_removes_new_borrow_source
    {s s' : State} (hStep : Step s .closeRegistry s') :
    s'.snapshot = [] := by
  cases hStep
  rfl

theorem retire_requires_no_borrow
    {s s' : State} {slot : SlotId} {generation : Generation}
    (hStep : Step s (.retirePublication slot generation) s') :
    s.findBorrowFor? slot generation = none := by
  cases hStep with
  | retirePublication _ _ _ hNoBorrow => exact hNoBorrow

theorem finish_removal_requires_reclamation
    {s s' : State} (hStep : Step s .finishClose s') :
    s.borrows = [] ∧ s.publications = [] ∧ s.snapshot = [] := by
  cases hStep with
  | finishClose hNoBorrows hNoPublications hNoSnapshot _ =>
      exact ⟨hNoBorrows, hNoPublications, hNoSnapshot⟩

def CloseCertified (s : State) : Prop :=
  s.registry.closed = true ∧
  s.borrows = [] ∧
  s.publications = [] ∧
  s.snapshot = []

theorem close_certified_when_finished
    {s s' : State} (hStep : Step s .finishClose s') :
    CloseCertified s' := by
  cases hStep with
  | finishClose hNoBorrows hNoPublications hNoSnapshot hReg =>
      cases hReg with
      | finishClose hClosed =>
          exact ⟨hClosed, hNoBorrows, hNoPublications, hNoSnapshot⟩

end XlFnFormal.Handle.Registry.Snapshot
