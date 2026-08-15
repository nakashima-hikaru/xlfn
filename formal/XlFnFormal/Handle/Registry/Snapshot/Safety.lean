import XlFnFormal.Handle.Registry.Snapshot.Invariant
import XlFnFormal.Handle.Registry.Safety

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Registry.Snapshot

open XlFnFormal.Handle.Registry

theorem live_snapshot_implies_canonical_live_root
    {s : State}
    (hInv : s.Invariant)
    {b : SnapshotBinding}
    (hMem : b ∈ s.snapshot) :
    ∃ h : b.slot < s.registry.slots.length,
      s.registry.slots.get ⟨b.slot, h⟩ = .live b.generation :=
  hInv.2.2.2.2.2.1 b hMem

theorem stale_publication_cannot_start_fast_lookup
    {s : State} {readerId : Nat} {token : Token} {pub : Publication}
    (hPub : s.findPublication? token.slot token.generation = some pub)
    (hStale : pub.state = .stale) :
    ¬ ∃ s', Step s (.beginFastLookup readerId token) s' := by
  intro ⟨s', hStep⟩
  cases hStep with
  | beginFastLookup _ _ _ hPubStep hLiveStep _ =>
      rw [hPub] at hPubStep
      injection hPubStep with hEq
      subst hEq
      rw [hStale] at hLiveStep
      contradiction

theorem closing_publication_cannot_start_fast_lookup
    {s : State} {readerId : Nat} {token : Token} {pub : Publication}
    (hPub : s.findPublication? token.slot token.generation = some pub)
    (hClosing : pub.state = .closing) :
    ¬ ∃ s', Step s (.beginFastLookup readerId token) s' := by
  intro ⟨s', hStep⟩
  cases hStep with
  | beginFastLookup _ _ _ hPubStep hLiveStep _ =>
      rw [hPub] at hPubStep
      injection hPubStep with hEq
      subst hEq
      rw [hClosing] at hLiveStep
      contradiction

theorem closed_registry_has_no_live_snapshot
    {s : State}
    (hInv : s.Invariant)
    (hClosed : s.registry.closed = true) :
    s.snapshot = [] :=
  (hInv.2.2.2.2.2.2.2.2 hClosed).2.1

theorem closed_registry_rejects_fast_lookup
    {s : State}
    (hInv : s.Invariant)
    (hClosed : s.registry.closed = true)
    (readerId : Nat) (token : Token) :
    ¬ ∃ s', Step s (.beginFastLookup readerId token) s' := by
  intro ⟨s', hStep⟩
  cases hStep with
  | beginFastLookup _ hSnap _ _ _ _ =>
      have hSnapNil := closed_registry_has_no_live_snapshot hInv hClosed
      have hSnapMem := List.mem_of_find?_eq_some hSnap
      rw [hSnapNil] at hSnapMem
      contradiction

theorem fast_lookup_linearizes_to_registry_begin_lookup
    {s s' : State} {readerId : Nat} {token : Token}
    (hStep : Step s (.beginFastLookup readerId token) s') :
    Registry.Step s.registry (.beginLookup token) s'.registry := by
  cases hStep
  assumption

theorem complete_fast_lookup_linearizes_to_registry_end_lookup
    {s s' : State} {readerId : Nat}
    (hStep : Step s (.completeFastLookup readerId) s') :
    Registry.Step s.registry .endLookup s'.registry := by
  cases hStep
  assumption

theorem fallback_fast_lookup_releases_lease
    {s s' : State} {readerId : Nat}
    (hStep : Step s (.fallbackFastLookup readerId) s') :
    Registry.Step s.registry .endLookup s'.registry := by
  cases hStep
  assumption

theorem stale_lookup_cannot_follow_reused_generation
    {s : State} {slot : SlotId} {oldGen newGen : Generation} {token : Token}
    (hGenNe : oldGen ≠ newGen)
    (hTokenSlot : token.slot = slot)
    (hTokenGen : token.generation = oldGen)
    (hBinding : s.findSnapshot? slot = some ⟨slot, newGen⟩) :
    (¬ ∃ readerId s', Step s (.beginFastLookup readerId token) s') ∧
    (∀ pub, s.findPublication? slot newGen = some pub → pub.generation ≠ token.generation) := by
  refine ⟨?_, ?_⟩
  · intro ⟨readerId, s', hStep⟩
    cases hStep with
    | beginFastLookup _ hSnap hSnapGen _ _ _ =>
        rw [hTokenSlot] at hSnap
        rw [hBinding] at hSnap
        injection hSnap with hEq
        rw [← hEq] at hSnapGen
        dsimp at hSnapGen
        rw [hTokenGen] at hSnapGen
        exact hGenNe hSnapGen.symm
  · intro pub hPub
    have hProp := List.find?_some hPub
    have hBool : (pub.slot == slot && pub.generation == newGen) = true := hProp
    rw [Bool.and_eq_true] at hBool
    have hG := beq_iff_eq.mp hBool.2
    rw [hG, hTokenGen]
    exact Ne.symm hGenNe

theorem fast_lookup_prevents_finish_close
    {s : State}
    (hInv : s.Invariant)
    (hLookups : s.fastLookups.length > 0) :
    ¬ ∃ s', Step s .finishClose s' := by
  intro ⟨s', hStep⟩
  cases hStep with
  | finishClose hRegStep =>
      cases hRegStep with
      | finishClose _ hNoLeases =>
          have hLeaseAcc := hInv.2.2.2.2.2.2.2.1
          dsimp [State.LeaseAccounting] at hLeaseAcc
          rw [hNoLeases] at hLeaseAcc
          omega

theorem close_certified_when_finished
    {session : SessionId} {s s' : State}
    (hReach : Reachable (initialState session) s)
    (hStep : Step s .finishClose s') :
    Registry.CloseCertified s'.registry ∧ s'.fastLookups = [] := by
  cases hStep with
  | finishClose hReg =>
      have hInv := Reachable.invariant_preserved (initialInvariant session) hReach
      cases hReg with
      | finishClose hClosed hNoLeases =>
          have hNoLive := (hInv.2.2.2.2.2.2.2.2 hClosed).1
          have hLeaseAcc := hInv.2.2.2.2.2.2.2.1
          dsimp [State.LeaseAccounting] at hLeaseAcc
          rw [hNoLeases] at hLeaseAcc
          have hLookupsNil : s.fastLookups = [] := by
            cases hL : s.fastLookups with
            | nil => rfl
            | cons head tail =>
                have hLen : (head :: tail).length > 0 := by simp
                rw [hL] at hLeaseAcc
                omega
          exact ⟨⟨hClosed, hNoLeases, hNoLive⟩, hLookupsNil⟩

end XlFnFormal.Handle.Registry.Snapshot
