import XlFnFormal.Handle.Registry.Invariant

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Registry

theorem max_generation_has_no_successor :
    nextGeneration? maxGeneration = none := by
  dsimp [nextGeneration?, maxGeneration]

theorem retired_is_permanent
    {s t : State} {slot : SlotId}
    (hRetired : RetiredAt s slot)
    (hReach : Reachable s t) :
    RetiredAt t slot :=
  Reachable.retiredAt_preserved hRetired hReach

theorem closeSlot_live_advances
    {g nextGen : Generation} (hNext : nextGeneration? g = some nextGen) :
    closeSlot (.live g) = .vacant nextGen := by
  dsimp [closeSlot]
  rw [hNext]

theorem closeSlot_live_retires
    {g : Generation} (hExhausted : nextGeneration? g = none) :
    closeSlot (.live g) = .retired := by
  dsimp [closeSlot]
  rw [hExhausted]

theorem closeSlot_vacant_advances
    {g nextGen : Generation} (hNext : nextGeneration? g = some nextGen) :
    closeSlot (.vacant g) = .vacant nextGen := by
  dsimp [closeSlot]
  rw [hNext]

theorem closeSlot_vacant_retires
    {g : Generation} (hExhausted : nextGeneration? g = none) :
    closeSlot (.vacant g) = .retired := by
  dsimp [closeSlot]
  rw [hExhausted]

def CloseCertified (s : State) : Prop :=
  s.closed = true ∧
  s.activeLeases = 0 ∧
  NoLiveSlots s

theorem Step.closeCertified_of_finishClose
    {s s' : State}
    (hReach : Reachable (initialState s.session) s)
    (hStep : Step s .finishClose s') :
    CloseCertified s' := by
  cases hStep with
  | finishClose hClosed hNoLeases =>
      exact ⟨hClosed, hNoLeases,
        Reachable.noLiveSlots_when_closed hReach hClosed⟩

theorem successful_close_is_certified
    {session : SessionId} {s : State}
    (hReach : Reachable (initialState session) s)
    (hClosed : s.closed = true)
    (hNoLeases : s.activeLeases = 0) :
    CloseCertified s := by
  exact ⟨hClosed, hNoLeases,
    Reachable.noLiveSlots_when_closed hReach hClosed⟩

theorem mismatched_generation_cannot_lookup
    {s : State} {token : Token} {current : Generation}
    {hInBounds : token.slot < s.slots.length}
    (hLive : s.slots.get ⟨token.slot, hInBounds⟩ = .live current)
    (hGeneration : token.generation ≠ current) :
    ¬ ∃ s', Step s (.beginLookup token) s' := by
  intro ⟨s', hStep⟩
  cases hStep
  rename_i _ _ _ hLookupLive
  rw [hLive] at hLookupLive
  cases hLookupLive
  exact hGeneration rfl

theorem stale_generation_cannot_lookup
    {s : State} {token : Token} {current : Generation}
    {hInBounds : token.slot < s.slots.length}
    (hStale : token.generation < current)
    (hLive : s.slots.get ⟨token.slot, hInBounds⟩ = .live current) :
    ¬ ∃ s', Step s (.beginLookup token) s' := by
  have hNe : token.generation ≠ current := by
    intro hEq
    rw [hEq] at hStale
    exact Nat.lt_irrefl current hStale
  exact mismatched_generation_cannot_lookup hLive hNe

theorem removed_token_cannot_become_valid_again
    {s : State} {token : Token} {hInBounds : token.slot < s.slots.length}
    (hRetired : s.slots.get ⟨token.slot, hInBounds⟩ = .retired) :
    ¬ ∃ s', Step s (.beginLookup token) s' := by
  intro ⟨s', hStep⟩
  cases hStep
  rename_i _ _ _ hLive
  rw [hRetired] at hLive
  cases hLive

theorem exhausted_slot_is_permanently_retired
    {s s' : State} {token : Token}
    (hStep : Step s (.removeRetire token) s') :
    RetiredAt s' token.slot ∧
    (∀ t, Reachable s' t → RetiredAt t token.slot) := by
  cases hStep with
  | removeRetire hAuth hInBounds hLive hExhausted =>
      have hRetired : RetiredAt ({ s with slots := s.slots.set token.slot .retired }) token.slot := by
        refine ⟨?_, ?_⟩
        · simpa using hInBounds
        · simp
      exact ⟨hRetired, fun t hReach => retired_is_permanent hRetired hReach⟩

theorem registry_close_invalidates_all_tokens
    {s s' : State}
    (hStep : Step s .closeRegistry s') :
    ∀ token, ¬ TokenLive s' token := by
  cases hStep with
  | closeRegistry hNotClosed =>
      intro token hLive
      rcases hLive with ⟨_, ⟨hInBounds, hSlotLive⟩⟩
      exact noLiveSlots_contradiction (map_closeSlot_noLiveSlots s) hSlotLive

theorem remove_reuse_reinsert_prevents_aba
    (session : SessionId)
    (hMax : 1 < maxGeneration) :
    let s0 := initialState session
    let token1 : Token := { session := session, slot := 0, generation := 1 }
    let s1 := { s0 with slots := [.live 1] }
    let s2 := { s1 with slots := [.vacant 2] }
    let s3 := { s2 with slots := [.live 2] }
    Step s0 .insertFresh s1 ∧
    Step s1 (.removeReuse token1 2) s2 ∧
    Step s2 (.insertReuse 0 2) s3 ∧
    ¬ ∃ s', Step s3 (.beginLookup token1) s' := by
  intro s0 token1 s1 s2 s3
  have hNext : nextGeneration? 1 = some 2 := by
    dsimp [nextGeneration?]
    rw [if_pos hMax]
  have hStep1 : Step s0 .insertFresh s1 := Step.insertFresh (by rfl)
  have hInBounds1 : token1.slot < s1.slots.length := by dsimp [token1, s1, s0, initialState]; decide
  have hLive1 : s1.slots.get ⟨token1.slot, hInBounds1⟩ = .live token1.generation := by rfl
  have hAuth1 : s1.AuthenticatedFor token1 := by rfl
  have hStep2 : Step s1 (.removeReuse token1 2) s2 := Step.removeReuse hAuth1 hInBounds1 hLive1 hNext
  have hInBounds2 : 0 < s2.slots.length := by dsimp [s2, s1, s0, initialState]; decide
  have hVacant2 : s2.slots.get ⟨0, hInBounds2⟩ = .vacant 2 := by rfl
  have hStep3 : Step s2 (.insertReuse 0 2) s3 := Step.insertReuse (by rfl) hInBounds2 hVacant2
  refine ⟨hStep1, hStep2, hStep3, ?_⟩
  intro ⟨s', hLookup⟩
  cases hLookup
  rename_i hLive3
  dsimp [s3, token1] at hLive3
  contradiction

end XlFnFormal.Handle.Registry
