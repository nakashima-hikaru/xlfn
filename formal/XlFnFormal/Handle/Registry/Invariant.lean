import XlFnFormal.Handle.Registry.Transition

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Registry

theorem closeSlot_not_live (slot : SlotState) :
    ¬ (closeSlot slot).IsLive := by
  cases slot with
  | vacant generation =>
      cases hNext : nextGeneration? generation <;>
        simp [closeSlot, hNext, SlotState.IsLive]
  | live generation =>
      cases hNext : nextGeneration? generation <;>
        simp [closeSlot, hNext, SlotState.IsLive]
  | retired =>
      simp [closeSlot, SlotState.IsLive]

theorem map_closeSlot_noLiveSlots (s : State) :
    NoLiveSlots { s with slots := s.slots.map closeSlot } := by
  intro slot hInBounds
  have hInBounds' : slot < s.slots.length := by
    simpa using hInBounds
  simp only [List.get_eq_getElem]
  rw [List.getElem_map]
  exact closeSlot_not_live (s.slots.get ⟨slot, hInBounds'⟩)

theorem noLiveSlots_set
    {s : State} {slot : SlotId} {replacement : SlotState}
    (hNoLive : NoLiveSlots s)
    (hReplacement : ¬ replacement.IsLive) :
    NoLiveSlots { s with slots := s.slots.set slot replacement } := by
  intro index hInBounds
  have hInBounds' : index < s.slots.length := by
    simpa using hInBounds
  simp only [List.get_eq_getElem]
  by_cases hEq : index = slot
  · subst index
    rw [List.getElem_set_self]
    exact hReplacement
  · rw [List.getElem_set_ne (Ne.symm hEq)]
    exact hNoLive index hInBounds'

theorem noLiveSlots_contradiction
    {s : State} {slot : SlotId} {generation : Generation}
    (hNoLive : NoLiveSlots s)
    {hInBounds : slot < s.slots.length}
    (hLive : s.slots.get ⟨slot, hInBounds⟩ = .live generation) :
    False := by
  have hNotLive := hNoLive slot hInBounds
  rw [hLive] at hNotLive
  exact hNotLive trivial

def RetiredAt (s : State) (slot : SlotId) : Prop :=
  ∃ h : slot < s.slots.length, s.slots.get ⟨slot, h⟩ = .retired

theorem Step.closedNoLiveInvariant_preserved
    {s s' : State} {e : Event}
    (hInv : s.closed = true → NoLiveSlots s)
    (hStep : Step s e s') :
    s'.closed = true → NoLiveSlots s' := by
  intro hClosed
  cases hStep with
  | insertFresh hMay =>
      simp_all [State.MayInsert]
  | insertReuse hMay hInBounds hVacant =>
      simp_all [State.MayInsert]
  | removeReuse hAuth hInBounds hLive hNextGen =>
      exact noLiveSlots_set (hInv hClosed) (by simp [SlotState.IsLive])
  | removeRetire hAuth hInBounds hLive hExhausted =>
      exact noLiveSlots_set (hInv hClosed) (by simp [SlotState.IsLive])
  | beginLookup =>
      exact hInv hClosed
  | endLookup =>
      exact hInv hClosed
  | closeRegistry hNotClosed =>
      exact map_closeSlot_noLiveSlots s
  | finishClose =>
      exact hInv hClosed

theorem initial_closedNoLiveInvariant (session : SessionId) :
    (initialState session).closed = true → NoLiveSlots (initialState session) := by
  intro hClosed
  simp [initialState] at hClosed

theorem Reachable.closedNoLiveInvariant_preserved
    {s t : State}
    (hInv : s.closed = true → NoLiveSlots s)
    (hReach : Reachable s t) :
    t.closed = true → NoLiveSlots t := by
  induction hReach with
  | refl => exact hInv
  | tail _ hStep ih => exact Step.closedNoLiveInvariant_preserved ih hStep

theorem Reachable.noLiveSlots_when_closed
    {session : SessionId} {s : State}
    (hReach : Reachable (initialState session) s)
    (hClosed : s.closed = true) :
    NoLiveSlots s := by
  exact Reachable.closedNoLiveInvariant_preserved
    (initial_closedNoLiveInvariant session) hReach hClosed

theorem Step.retiredAt_preserved
    {s s' : State} {e : Event} {slot : SlotId}
    (hRetired : RetiredAt s slot)
    (hStep : Step s e s') :
    RetiredAt s' slot := by
  rcases hRetired with ⟨hInBounds, hState⟩
  simp only [List.get_eq_getElem] at hState
  cases hStep with
  | insertFresh =>
      have hInBounds' : slot < (s.slots ++ [SlotState.live 1]).length := by
        rw [List.length_append]; exact Nat.lt_add_right 1 hInBounds
      refine ⟨hInBounds', ?_⟩
      simp only [List.get_eq_getElem]
      rw [List.getElem_append_left hInBounds]
      exact hState
  | insertReuse hMay hInBounds' hVacant =>
      rename_i slot' gen
      have hLen : (s.slots.set slot' (SlotState.live gen)).length = s.slots.length := List.length_set
      have hInBounds'' : slot < (s.slots.set slot' (SlotState.live gen)).length := by rw [hLen]; exact hInBounds
      refine ⟨hInBounds'', ?_⟩
      simp only [List.get_eq_getElem]
      by_cases hEq : slot = slot'
      · subst hEq
        simp only [List.get_eq_getElem] at hVacant
        rw [hState] at hVacant
        contradiction
      · rw [List.getElem_set_ne (Ne.symm hEq)]
        exact hState
  | removeReuse hAuth hInBounds' hLive hNextGen =>
      rename_i token nextGen
      have hLen : (s.slots.set token.slot (SlotState.vacant nextGen)).length = s.slots.length := List.length_set
      have hInBounds'' : slot < (s.slots.set token.slot (SlotState.vacant nextGen)).length := by rw [hLen]; exact hInBounds
      refine ⟨hInBounds'', ?_⟩
      simp only [List.get_eq_getElem]
      by_cases hEq : slot = token.slot
      · subst hEq
        simp only [List.get_eq_getElem] at hLive
        rw [hState] at hLive
        contradiction
      · rw [List.getElem_set_ne (Ne.symm hEq)]
        exact hState
  | removeRetire hAuth hInBounds' hLive hExhausted =>
      rename_i token
      have hLen : (s.slots.set token.slot SlotState.retired).length = s.slots.length := List.length_set
      have hInBounds'' : slot < (s.slots.set token.slot SlotState.retired).length := by rw [hLen]; exact hInBounds
      refine ⟨hInBounds'', ?_⟩
      simp only [List.get_eq_getElem]
      by_cases hEq : slot = token.slot
      · subst hEq
        exact List.getElem_set_self _
      · rw [List.getElem_set_ne (Ne.symm hEq)]
        exact hState
  | beginLookup => exact ⟨hInBounds, by simp only [List.get_eq_getElem]; exact hState⟩
  | endLookup => exact ⟨hInBounds, by simp only [List.get_eq_getElem]; exact hState⟩
  | closeRegistry =>
      have hLen : (s.slots.map closeSlot).length = s.slots.length := List.length_map _
      have hInBounds' : slot < (s.slots.map closeSlot).length := by rw [hLen]; exact hInBounds
      refine ⟨hInBounds', ?_⟩
      simp only [List.get_eq_getElem]
      rw [List.getElem_map]
      dsimp [closeSlot]
      rw [hState]
  | finishClose => exact ⟨hInBounds, by simp only [List.get_eq_getElem]; exact hState⟩

theorem Reachable.retiredAt_preserved
    {s t : State} {slot : SlotId}
    (hRetired : RetiredAt s slot)
    (hReach : Reachable s t) :
    RetiredAt t slot := by
  induction hReach with
  | refl => exact hRetired
  | tail _ hStep ih => exact Step.retiredAt_preserved ih hStep

end XlFnFormal.Handle.Registry
