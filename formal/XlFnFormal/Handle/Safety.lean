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

def aba_s1 (session : SessionId) : State :=
  { session := session, phase := .«open», slots := [.live 1], activePrepares := 0, activeLeases := 0 }

def aba_s2 (session : SessionId) : State :=
  { session := session, phase := .«open», slots := [.vacant 2], activePrepares := 0, activeLeases := 0 }

def aba_s3 (session : SessionId) : State :=
  { session := session, phase := .«open», slots := [.live 2], activePrepares := 0, activeLeases := 0 }

theorem Step_aba_step1 (session : SessionId) :
    Step (State.initialState session) Event.insertFresh (aba_s1 session) := by
  have hPhase : (State.initialState session).phase = .«open» ∨ (State.initialState session).phase = .drainingPrepares := Or.inl rfl
  exact Step.insertFresh hPhase

theorem Step_aba_step2 (session : SessionId) :
    Step (aba_s1 session) (Event.removeReuse { session := session, slot := 0, generation := 1 } 2) (aba_s2 session) := by
  have hAuth : (aba_s1 session).AuthenticatedFor { session := session, slot := 0, generation := 1 } := rfl
  have hInBounds : 0 < (aba_s1 session).slots.length := by dsimp [aba_s1]; decide
  have hLive : (aba_s1 session).slots.get ⟨0, hInBounds⟩ = .live 1 := rfl
  have hNextGen : nextGeneration? 1 = some 2 := rfl
  exact Step.removeReuse hAuth hInBounds hLive hNextGen

theorem Step_aba_step3 (session : SessionId) :
    Step (aba_s2 session) (Event.insertReuse 0 2) (aba_s3 session) := by
  have hPhase : (aba_s2 session).phase = .«open» ∨ (aba_s2 session).phase = .drainingPrepares := Or.inl rfl
  have hInBounds : 0 < (aba_s2 session).slots.length := by dsimp [aba_s2]; decide
  have hVacant : (aba_s2 session).slots.get ⟨0, hInBounds⟩ = .vacant 2 := rfl
  exact Step.insertReuse hPhase hInBounds hVacant

theorem remove_reuse_reinsert_prevents_aba
    (session : SessionId) :
    ∃ s1 s2 s3,
      Step (State.initialState session) .insertFresh s1 ∧
      Step s1
        (.removeReuse
          { session := session, slot := 0, generation := 1 }
          2)
        s2 ∧
      Step s2 (.insertReuse 0 2) s3 ∧
      ¬ ∃ s4,
        Step s3
          (.beginLookup
            { session := session, slot := 0, generation := 1 })
          s4 := by
  have hInBounds3 : 0 < (aba_s3 session).slots.length := by dsimp [aba_s3]; decide
  have hLive3 : (aba_s3 session).slots.get ⟨0, hInBounds3⟩ = .live 2 := rfl
  refine ⟨aba_s1 session, aba_s2 session, aba_s3 session, ?_, ?_, ?_, ?_⟩
  · exact Step_aba_step1 session
  · exact Step_aba_step2 session
  · exact Step_aba_step3 session
  · exact aba_reuse_prevents_stale_token_lookup hInBounds3 hLive3 rfl

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
    (hStep : Step s (.removeRetire token) s')
    (hInBounds : token.slot < s'.slots.length) :
    s'.slots.get ⟨token.slot, hInBounds⟩ = .retired ∧
    (∀ gen s'', ¬ Step s' (.insertReuse token.slot gen) s'') ∧
    (∀ s'', ¬ Step s' (.beginLookup token) s'') := by
  cases hStep with
  | removeRetire hAuth hInBoundsOrig hLive hExhausted =>
      have hGet : (s.slots.set token.slot .retired).get ⟨token.slot, hInBounds⟩ = .retired := by
        simp
      refine ⟨hGet, ?_, ?_⟩
      · intro gen s'' hInsert
        cases hInsert
        rename_i _ _ hVacant
        rw [hGet] at hVacant
        cases hVacant
      · intro s'' hLookup
        cases hLookup
        rename_i _ _ _ hLookupLive
        rw [hGet] at hLookupLive
        cases hLookupLive

theorem registry_close_invalidates_all_tokens
    {init s : State} {token : Token} (hReach : Reachable init s) (hInvInit : PhaseInvariant init)
    (hClosed : s.phase = .registryClosed ∨ s.phase = .closed) :
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
