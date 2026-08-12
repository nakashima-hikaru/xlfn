import XlFnFormal.Handle.Invariant

set_option autoImplicit false

namespace XlFnFormal.Handle

theorem get_append_left_slot (xs ys : List SlotState) (i : Nat) (h1 : i < xs.length) (h2 : i < (xs ++ ys).length) :
    (xs ++ ys).get ⟨i, h2⟩ = xs.get ⟨i, h1⟩ := by
  induction xs generalizing i with
  | nil => contradiction
  | cons x xs ih =>
      cases i with
      | zero => rfl
      | succ i =>
          have h1' : i < xs.length := Nat.lt_of_succ_lt_succ h1
          have h2' : i < (xs ++ ys).length := Nat.lt_of_succ_lt_succ h2
          exact ih i h1' h2'

theorem get_set_ne_slot (xs : List SlotState) (i j : Nat) (v : SlotState) (hj : j < xs.length) (hj' : j < (xs.set i v).length) (hNe : j ≠ i) :
    (xs.set i v).get ⟨j, hj'⟩ = xs.get ⟨j, hj⟩ := by
  induction xs generalizing i j with
  | nil => contradiction
  | cons x xs ih =>
      cases i with
      | zero =>
          cases j with
          | zero => contradiction
          | succ j => rfl
      | succ i =>
          cases j with
          | zero => rfl
          | succ j =>
              have hj1 : j < xs.length := Nat.lt_of_succ_lt_succ hj
              have hj'1 : j < (xs.set i v).length := Nat.lt_of_succ_lt_succ hj'
              have hNe' : j ≠ i := fun h => hNe (congrArg Nat.succ h)
              exact ih i j hj1 hj'1 hNe'

theorem get_set_eq_slot (xs : List SlotState) (i : Nat) (v : SlotState) (hi : i < xs.length) (hi' : i < (xs.set i v).length) :
    (xs.set i v).get ⟨i, hi'⟩ = v := by
  induction xs generalizing i with
  | nil => contradiction
  | cons x xs ih =>
      cases i with
      | zero => rfl
      | succ i =>
          have hi1 : i < xs.length := Nat.lt_of_succ_lt_succ hi
          have hi'1 : i < (xs.set i v).length := Nat.lt_of_succ_lt_succ hi'
          exact ih i hi1 hi'1

theorem get_map_closeSlot (xs : List SlotState) (i : Nat) (hi : i < xs.length) (hi' : i < (xs.map closeSlot).length) :
    (xs.map closeSlot).get ⟨i, hi'⟩ = closeSlot (xs.get ⟨i, hi⟩) := by
  induction xs generalizing i with
  | nil => contradiction
  | cons x xs ih =>
      cases i with
      | zero => rfl
      | succ i =>
          have hi1 : i < xs.length := Nat.lt_of_succ_lt_succ hi
          have hi'1 : i < (xs.map closeSlot).length := Nat.lt_of_succ_lt_succ hi'
          exact ih i hi1 hi'1

def RetiredAt (s : State) (slot : SlotId) : Prop :=
  ∃ hIn : slot < s.slots.length, s.slots.get ⟨slot, hIn⟩ = SlotState.retired

theorem Step.retiredAt_preserved
    {s s' : State} {e : Event} {slot : SlotId}
    (hRetired : RetiredAt s slot)
    (hStep : Step s e s') :
    RetiredAt s' slot := by
  unfold RetiredAt at *
  rcases hRetired with ⟨hIn, hGet⟩
  cases hStep with
  | beginPrepare => exact ⟨hIn, hGet⟩
  | endPrepare => exact ⟨hIn, hGet⟩
  | beginInitialize => exact ⟨hIn, hGet⟩
  | finishInitialize => exact ⟨hIn, hGet⟩
  | insertPendingFresh =>
      have hIn' : slot < (s.slots ++ [SlotState.live 1]).length := by
        rw [List.length_append]
        exact Nat.lt_add_right 1 hIn
      have hGet' : (s.slots ++ [SlotState.live 1]).get ⟨slot, hIn'⟩ = SlotState.retired := by
        rw [get_append_left_slot s.slots [SlotState.live 1] slot hIn hIn']
        exact hGet
      exact ⟨hIn', hGet'⟩
  | insertPendingReuse hFind hInBounds hVacant =>
      rename_i id slotId gen
      by_cases hEq : slot = slotId
      · subst hEq
        have hEq2 : SlotState.retired = SlotState.vacant gen := hGet.symm.trans hVacant
        cases hEq2
      · have hIn' : slot < (s.slots.set slotId (SlotState.live gen)).length := by
          rw [List.length_set]
          exact hIn
        have hGet' : (s.slots.set slotId (SlotState.live gen)).get ⟨slot, hIn'⟩ = SlotState.retired := by
          rw [get_set_ne_slot s.slots slotId slot (SlotState.live gen) hIn hIn' hEq]
          exact hGet
        exact ⟨hIn', hGet'⟩
  | publishTopic => exact ⟨hIn, hGet⟩
  | rollbackPendingReuse hFind hInBounds hLive hNextGen =>
      rename_i id token nextGen
      by_cases hEq : slot = token.slot
      · subst hEq
        have hEq2 : SlotState.retired = SlotState.live token.generation := hGet.symm.trans hLive
        cases hEq2
      · have hIn' : slot < (s.slots.set token.slot (SlotState.vacant nextGen)).length := by
          rw [List.length_set]
          exact hIn
        have hGet' : (s.slots.set token.slot (SlotState.vacant nextGen)).get ⟨slot, hIn'⟩ = SlotState.retired := by
          rw [get_set_ne_slot s.slots token.slot slot (SlotState.vacant nextGen) hIn hIn' hEq]
          exact hGet
        exact ⟨hIn', hGet'⟩
  | rollbackPendingRetire hFind hInBounds hLive hExhausted =>
      rename_i id token
      by_cases hEq : slot = token.slot
      · subst hEq
        have hIn' : token.slot < (s.slots.set token.slot SlotState.retired).length := by
          rw [List.length_set]
          exact hIn
        have hGet' : (s.slots.set token.slot SlotState.retired).get ⟨token.slot, hIn'⟩ = SlotState.retired := by
          exact get_set_eq_slot s.slots token.slot SlotState.retired hIn hIn'
        exact ⟨hIn', hGet'⟩
      · have hIn' : slot < (s.slots.set token.slot SlotState.retired).length := by
          rw [List.length_set]
          exact hIn
        have hGet' : (s.slots.set token.slot SlotState.retired).get ⟨slot, hIn'⟩ = SlotState.retired := by
          rw [get_set_ne_slot s.slots token.slot slot SlotState.retired hIn hIn' hEq]
          exact hGet
        exact ⟨hIn', hGet'⟩
  | removeReuse hAuth hInBounds hLive hNextGen =>
      rename_i token nextGen
      by_cases hEq : slot = token.slot
      · subst hEq
        have hEq2 : SlotState.retired = SlotState.live token.generation := hGet.symm.trans hLive
        cases hEq2
      · have hIn' : slot < (s.slots.set token.slot (SlotState.vacant nextGen)).length := by
          rw [List.length_set]
          exact hIn
        have hGet' : (s.slots.set token.slot (SlotState.vacant nextGen)).get ⟨slot, hIn'⟩ = SlotState.retired := by
          rw [get_set_ne_slot s.slots token.slot slot (SlotState.vacant nextGen) hIn hIn' hEq]
          exact hGet
        exact ⟨hIn', hGet'⟩
  | removeRetire hAuth hInBounds hLive hExhausted =>
      rename_i token
      by_cases hEq : slot = token.slot
      · subst hEq
        have hIn' : token.slot < (s.slots.set token.slot SlotState.retired).length := by
          rw [List.length_set]
          exact hIn
        have hGet' : (s.slots.set token.slot SlotState.retired).get ⟨token.slot, hIn'⟩ = SlotState.retired := by
          exact get_set_eq_slot s.slots token.slot SlotState.retired hIn hIn'
        exact ⟨hIn', hGet'⟩
      · have hIn' : slot < (s.slots.set token.slot SlotState.retired).length := by
          rw [List.length_set]
          exact hIn
        have hGet' : (s.slots.set token.slot SlotState.retired).get ⟨slot, hIn'⟩ = SlotState.retired := by
          rw [get_set_ne_slot s.slots token.slot slot SlotState.retired hIn hIn' hEq]
          exact hGet
        exact ⟨hIn', hGet'⟩
  | beginLookup => exact ⟨hIn, hGet⟩
  | endLookup => exact ⟨hIn, hGet⟩
  | sealTopics => exact ⟨hIn, hGet⟩
  | closeRegistry =>
      have hIn' : slot < (s.slots.map closeSlot).length := by
        rw [List.length_map]
        exact hIn
      have hGet' : (s.slots.map closeSlot).get ⟨slot, hIn'⟩ = SlotState.retired := by
        rw [get_map_closeSlot s.slots slot hIn hIn']
        rw [hGet]
        rfl
      exact ⟨hIn', hGet'⟩
  | finishClose => exact ⟨hIn, hGet⟩

theorem retired_is_permanent
    {s t : State} {slot : SlotId}
    (hRetired : RetiredAt s slot)
    (hReach : Reachable s t) :
    RetiredAt t slot := by
  induction hReach with
  | init => exact hRetired
  | step _ hStep ih => exact Step.retiredAt_preserved ih hStep

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

theorem exhausted_slot_is_permanently_retired
    {s s' : State} {token : Token}
    (hStep : Step s (.removeRetire token) s')
    (hInBounds : token.slot < s'.slots.length) :
    s'.slots.get ⟨token.slot, hInBounds⟩ = .retired ∧
    (∀ id gen s'', ¬ Step s' (.insertPendingReuse id token.slot gen) s'') ∧
    (∀ s'', ¬ Step s' (.beginLookup token) s'') := by
  cases hStep with
  | removeRetire hAuth hInBoundsOrig hLive hExhausted =>
      have hGet : (s.slots.set token.slot .retired).get ⟨token.slot, hInBounds⟩ = .retired := by
        simp
      refine ⟨hGet, ?_, ?_⟩
      · intro id gen s'' hInsert
        cases hInsert
        rename_i hFind hInBoundsNew hVacant
        rw [hGet] at hVacant
        cases hVacant
      · intro s'' hLookup
        cases hLookup
        rename_i hPhase hAuth hInBoundsNew hLookupLive
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
  exact hInv.2.2.2

theorem successful_close_is_quiescent
    (session : SessionId) (s : State)
    (hReach : Reachable (State.initialState session) s)
    (hClosed : s.phase = .closed) :
    s.CloseCertified := by
  have hInitInv : PhaseInvariant (State.initialState session) := trivial
  have hInv := reachable_phaseInvariant hReach hInitInv
  unfold PhaseInvariant at hInv
  rw [hClosed] at hInv
  exact ⟨hClosed, hInv.2.1, hInv.2.2.1, hInv.2.2.2, hInv.1⟩

theorem no_topic_publication_after_seal
    {s : State} {id : InitializerId} (hSealed : s.phase = .drainingPrepares) :
    ¬ ∃ s', Step s (.publishTopic id) s' := by
  intro ⟨s', hStep⟩
  cases hStep
  rename_i hPhase _
  rw [hSealed] at hPhase
  cases hPhase

theorem registry_close_waits_for_initializers
    {s s' : State} (hStep : Step s .closeRegistry s') :
    s.initializers = [] ∧ s.activePrepares = 0 := by
  cases hStep
  exact ⟨by assumption, by assumption⟩

theorem draining_pending_insert_cannot_escape
    {s : State} {id : InitializerId} {token : Token}
    (hSealed : s.phase = .drainingPrepares)
    (hFind : s.findInitializer? id = some { id := id, stage := .pending token })
    (hInBounds : token.slot < s.slots.length)
    (hLive : s.slots.get ⟨token.slot, hInBounds⟩ = .live token.generation) :
    (¬ ∃ s', Step s (.publishTopic id) s') ∧
    ((∃ nextGen s', Step s (.rollbackPendingReuse id nextGen) s') ∨
     (∃ s', Step s (.rollbackPendingRetire id) s')) := by
  refine ⟨no_topic_publication_after_seal hSealed, ?_⟩
  cases hNext : nextGeneration? token.generation with
  | some nextGen =>
      left
      refine ⟨nextGen, _, Step.rollbackPendingReuse hFind hInBounds hLive hNext⟩
  | none =>
      right
      refine ⟨_, Step.rollbackPendingRetire hFind hInBounds hLive hNext⟩

end XlFnFormal.Handle
