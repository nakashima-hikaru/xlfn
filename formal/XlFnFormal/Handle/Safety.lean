import XlFnFormal.Handle.Invariant

set_option autoImplicit false

namespace XlFnFormal.Handle

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

theorem aba_reuse_prevents_stale_token_lookup
    {s2 : State} {token1 : Token}
    (hInBounds2 : token1.slot < s2.slots.length)
    (hLive2 : s2.slots.get ⟨token1.slot, hInBounds2⟩ = .live 2)
    (hStale : token1.generation = 1) :
    ¬ ∃ s', Step s2 (.beginLookup token1) s' := by
  have hNe : token1.generation ≠ 2 := by rw [hStale]; decide
  exact mismatched_generation_cannot_lookup hLive2 hNe

theorem removed_token_cannot_become_valid_again
    {s : State} {token : Token} {hInBounds : token.slot < s.slots.length}
    (hRetired : s.slots.get ⟨token.slot, hInBounds⟩ = .retired) :
    ¬ ∃ s', Step s (.beginLookup token) s' := by
  intro ⟨s', hStep⟩
  cases hStep
  rename_i _ _ _ hLive
  rw [hRetired] at hLive
  cases hLive

theorem registry_close_invalidates_all_tokens
    {init s : State} {token : Token} (hReach : Reachable init s) (hInvInit : PhaseInvariant init)
    (hClosed : s.phase = .registryClosed ∨ s.phase = .closed)
    (_hInBounds : token.slot < s.slots.length) :
    ¬ ∃ s', Step s (.beginLookup token) s' := by
  have hInv := reachable_phaseInvariant hReach hInvInit
  cases hClosed with
  | inl hRC =>
      unfold PhaseInvariant at hInv
      rw [hRC] at hInv
      intro ⟨s', hStep⟩
      cases hStep
      rename_i _ _ _ hLive
      exact noLiveSlots_contradiction hInv.1 hLive
  | inr hC =>
      unfold PhaseInvariant at hInv
      rw [hC] at hInv
      intro ⟨s', hStep⟩
      cases hStep
      rename_i _ _ _ hLive
      exact noLiveSlots_contradiction hInv.1 hLive

theorem certified_close_has_no_outstanding_leases
    {init s : State} (hReach : Reachable init s) (hInvInit : PhaseInvariant init)
    (hClosed : s.phase = .closed) :
    s.activeLeases = 0 := by
  have hInv := reachable_phaseInvariant hReach hInvInit
  unfold PhaseInvariant at hInv
  rw [hClosed] at hInv
  exact hInv.2.2

theorem successful_close_is_quiescent
    (session : SessionId) (s : State)
    (hReach : Reachable (State.initialState session) s)
    (hClosed : s.phase = .closed) :
    s.CloseCertified := by
  have hInitInv : PhaseInvariant (State.initialState session) := trivial
  have hInv := reachable_phaseInvariant hReach hInitInv
  unfold PhaseInvariant at hInv
  rw [hClosed] at hInv
  exact ⟨hClosed, hInv.2.1, hInv.2.2, hInv.1⟩

end XlFnFormal.Handle
