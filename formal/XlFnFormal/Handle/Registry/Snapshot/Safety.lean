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

theorem stale_publication_cannot_start_observation
    {s : State} {readerId : Nat} {token : Token} {pub : Publication}
    (hPub : s.findPublication? token.slot token.generation = some pub)
    (hStale : pub.state = .stale) :
    ¬ ∃ s', Step s (.beginFastObservation readerId token) s' := by
  intro ⟨s', hStep⟩
  cases hStep with
  | beginFastObservation _ _ _ hPubStep _ hLiveStep =>
      rw [hPub] at hPubStep
      injection hPubStep with hEq
      subst hEq
      rw [hStale] at hLiveStep
      contradiction

theorem closing_publication_cannot_start_observation
    {s : State} {readerId : Nat} {token : Token} {pub : Publication}
    (hPub : s.findPublication? token.slot token.generation = some pub)
    (hClosing : pub.state = .closing) :
    ¬ ∃ s', Step s (.beginFastObservation readerId token) s' := by
  intro ⟨s', hStep⟩
  cases hStep with
  | beginFastObservation _ _ _ hPubStep _ hLiveStep =>
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

theorem closed_registry_is_sealed
    {s : State}
    (hInv : s.Invariant)
    (hClosed : s.registry.closed = true) :
    s.leaseAdmission = .sealed :=
  (hInv.2.2.2.2.2.2.2.2 hClosed).2.2.2

theorem closed_registry_rejects_observation
    {s : State}
    (hInv : s.Invariant)
    (hClosed : s.registry.closed = true)
    (readerId : Nat) (token : Token) :
    ¬ ∃ s', Step s (.beginFastObservation readerId token) s' := by
  intro ⟨s', hStep⟩
  cases hStep with
  | beginFastObservation _ hSnap _ _ _ _ =>
      have hSnapNil := closed_registry_has_no_live_snapshot hInv hClosed
      have hSnapMem := List.mem_of_find?_eq_some hSnap
      rw [hSnapNil] at hSnapMem
      contradiction

theorem sealed_admission_rejects_tentative_lease_acquisition
    {s : State}
    (hSealed : s.leaseAdmission = .sealed)
    (readerId : Nat) :
    ¬ ∃ s', Step s (.acquireTentativeLease readerId) s' := by
  intro ⟨s', hStep⟩
  cases hStep with
  | acquireTentativeLease _ _ hNotSealed _ =>
      exact hNotSealed hSealed

theorem sealed_admission_rejects_slow_lookup
    {s : State}
    (hSealed : s.leaseAdmission = .sealed)
    (token : Token) :
    ¬ ∃ s', Step s (.beginSlowLookup token) s' := by
  intro ⟨s', hStep⟩
  cases hStep with
  | beginSlowLookup hNotSealed _ =>
      exact hNotSealed hSealed

theorem close_registry_requires_sealed_admission
    {s s' : State}
    (hStep : Step s .closeRegistry s') :
    s.leaseAdmission = .sealed := by
  cases hStep with
  | closeRegistry hSealed _ =>
      exact hSealed

theorem closed_registry_rejects_tentative_lease_acquisition
    {s : State}
    (hClosed : s.registry.closed = true)
    (readerId : Nat) :
    ¬ ∃ s', Step s (.acquireTentativeLease readerId) s' := by
  intro ⟨s', hStep⟩
  cases hStep with
  | acquireTentativeLease _ _ _ hNotClosed =>
      rw [hClosed] at hNotClosed
      contradiction

theorem validate_fast_lookup_linearizes_to_registry_begin_lookup
    {s s' : State} {readerId : Nat}
    (hStep : Step s (.validateFastLookup readerId) s') :
    ∃ lookup, s.findFastLookup? readerId = some lookup ∧
      Registry.Step s.registry (.beginLookup lookup.token) s'.registry := by
  cases hStep with
  | validateFastLookup hLookup _ _ _ hReg =>
      exact ⟨_, hLookup, hReg⟩

theorem reject_tentative_fast_lookup_requires_non_live
    {s s' : State} {readerId : Nat}
    (hStep : Step s (.rejectTentativeFastLookup readerId) s') :
    ∃ lookup pub,
      s.findFastLookup? readerId = some lookup ∧
      lookup.stage = .tentative ∧
      s.findPublication? lookup.token.slot lookup.token.generation = some pub ∧
      pub.state ≠ .live := by
  cases hStep with
  | rejectTentativeFastLookup hLookup hTentative hPub hNotLive =>
      exact ⟨_, _, hLookup, hTentative, hPub, hNotLive⟩

theorem complete_fast_lookup_linearizes_to_registry_end_lookup
    {s s' : State} {readerId : Nat}
    (hStep : Step s (.completeFastLookup readerId) s') :
    Registry.Step s.registry .endLookup s'.registry := by
  cases hStep
  assumption

theorem fallback_fast_lookup_requires_non_live_and_releases_lease
    {s s' : State} {readerId : Nat}
    (hStep : Step s (.fallbackFastLookup readerId) s') :
    ∃ lookup pub,
      s.findFastLookup? readerId = some lookup ∧
      lookup.stage = .validated ∧
      s.findPublication? lookup.token.slot lookup.token.generation = some pub ∧
      pub.state ≠ .live ∧
      Registry.Step s.registry .endLookup s'.registry := by
  cases hStep with
  | fallbackFastLookup hLookup hVal hPub hNotLive hReg =>
      exact ⟨_, _, hLookup, hVal, hPub, hNotLive, hReg⟩

theorem stale_lookup_cannot_follow_reused_generation
    {s : State} {slot : SlotId} {oldGen newGen : Generation} {token : Token}
    (hGenNe : oldGen ≠ newGen)
    (hTokenSlot : token.slot = slot)
    (hTokenGen : token.generation = oldGen)
    (hBinding : s.findSnapshot? slot = some ⟨slot, newGen⟩) :
    (¬ ∃ readerId s', Step s (.beginFastObservation readerId token) s') ∧
    (∀ pub, s.findPublication? slot newGen = some pub → pub.generation ≠ token.generation) := by
  refine ⟨?_, ?_⟩
  · intro ⟨readerId, s', hStep⟩
    cases hStep with
    | beginFastObservation _ hSnap hSnapGen _ _ _ =>
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

theorem tentative_fast_lookup_prevents_finish_close
    {s : State}
    (hLookups : s.tentativeFastLookups.length > 0) :
    ¬ ∃ s', Step s .finishClose s' := by
  intro ⟨s', hStep⟩
  cases hStep with
  | finishClose hNoTentative _ _ =>
      rw [hNoTentative] at hLookups
      contradiction

theorem validated_fast_lookup_prevents_finish_close
    {s : State}
    (hLookups : s.validatedFastLookups.length > 0) :
    ¬ ∃ s', Step s .finishClose s' := by
  intro ⟨s', hStep⟩
  cases hStep with
  | finishClose _ hNoValidated _ =>
      rw [hNoValidated] at hLookups
      contradiction

theorem close_certified_when_finished
    {session : SessionId} {s s' : State}
    (hReach : Reachable (initialState session) s)
    (hStep : Step s .finishClose s') :
    Registry.CloseCertified s'.registry ∧ s'.tentativeFastLookups = [] ∧ s'.validatedFastLookups = [] ∧ s'.leaseAdmission = .sealed := by
  cases hStep with
  | finishClose hNoTentative hNoValidated hReg =>
      have hInv := Reachable.invariant_preserved (initialInvariant session) hReach
      cases hReg with
      | finishClose hClosed hNoLeases =>
          have hNoLive := (hInv.2.2.2.2.2.2.2.2 hClosed).1
          have hSealed := (hInv.2.2.2.2.2.2.2.2 hClosed).2.2.2
          exact ⟨⟨hClosed, hNoLeases, hNoLive⟩, hNoTentative, hNoValidated, hSealed⟩

end XlFnFormal.Handle.Registry.Snapshot
