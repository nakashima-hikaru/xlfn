import XlFnFormal.Handle.Invariant

set_option autoImplicit false

namespace XlFnFormal.Handle

theorem stale_generation_cannot_lookup
    {s : State} {token : Token} {hInBounds : token.slot < s.slots.length} {g : Generation}
    (_hStale : token.generation < g)
    (hSlot : s.slots.get ⟨token.slot, hInBounds⟩ = .vacant g) :
    s.slots.get ⟨token.slot, hInBounds⟩ ≠ .live token.generation := by
  rw [hSlot]
  intro hEq
  cases hEq

theorem removed_token_cannot_become_valid_again
    {s : State} {token : Token} {hInBounds : token.slot < s.slots.length}
    (hRetired : s.slots.get ⟨token.slot, hInBounds⟩ = .retired) :
    s.slots.get ⟨token.slot, hInBounds⟩ ≠ .live token.generation := by
  rw [hRetired]
  intro hEq
  cases hEq

theorem registry_close_invalidates_all_tokens
    {init s : State} {token : Token} (hReach : Reachable init s) (hInvInit : PhaseInvariant init)
    (hClosed : s.phase = .registryClosed ∨ s.phase = .closed)
    (hInBounds : token.slot < s.slots.length) :
    s.slots.get ⟨token.slot, hInBounds⟩ ≠ .live token.generation := by
  have hInv := reachable_phaseInvariant hReach hInvInit
  cases hClosed with
  | inl hRC =>
      unfold PhaseInvariant at hInv
      rw [hRC] at hInv
      intro hLive
      exact noLiveSlots_contradiction hInv.1 hLive
  | inr hC =>
      unfold PhaseInvariant at hInv
      rw [hC] at hInv
      intro hLive
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
    (session : SessionId) (numSlots : Nat) (s : State)
    (hReach : Reachable (State.initialState session numSlots) s)
    (hClosed : s.phase = .closed) :
    s.CloseCertified := by
  have hInitInv : PhaseInvariant (State.initialState session numSlots) := trivial
  have hInv := reachable_phaseInvariant hReach hInitInv
  unfold PhaseInvariant at hInv
  rw [hClosed] at hInv
  exact ⟨hClosed, hInv.2.1, hInv.2.2, hInv.1⟩

end XlFnFormal.Handle
