import XlFnFormal.TemporalOwnership.Invariant

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.TemporalOwnership

theorem readerImpliesOwned
    {s : State} (hInv : s.Invariant) (hReaders : s.readers > 0) :
    s.ownerPresent = true :=
  hInv.2.1 hReaders

theorem publishedImpliesOwned
    {s : State} (hInv : s.Invariant) (hPub : s.published = true) :
    s.ownerPresent = true :=
  hInv.1 hPub

theorem reclaimedImpliesNoReaders
    {s : State} (hInv : s.Invariant) (hNotOwner : s.ownerPresent = false) :
    s.readers = 0 :=
  (hInv.2.2 hNotOwner).2

theorem sealedImpliesNoNewReaders
    {s : State} (hSealed : s.gate = .sealed) :
    ¬ ∃ s', Step s .enter s' := by
  intro ⟨s', hStep⟩
  cases hStep with
  | enter _ _ hOpen =>
      rw [hSealed] at hOpen
      contradiction

theorem reclaimRequiresUnpublishedAndDrained
    {s s' : State} (hStep : Step s .reclaim s') :
    s.gate = .sealed ∧ s.published = false ∧ s.readers = 0 := by
  cases hStep with
  | reclaim _ hSealed hNotPub hDrained =>
      exact ⟨hSealed, hNotPub, hDrained⟩

/-- The fundamental temporal ownership safety theorem:
    Any reachable state with active readers guarantees that the owner is present. -/
theorem noUseAfterReclaim
    {s : State} (hReach : Reachable initialState s) (hReaders : s.readers > 0) :
    s.ownerPresent = true :=
  readerImpliesOwned (reachable_invariant hReach) hReaders

end XlFnFormal.TemporalOwnership
