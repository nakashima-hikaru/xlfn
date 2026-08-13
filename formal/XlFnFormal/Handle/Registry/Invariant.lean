import XlFnFormal.Handle.Registry.Transition

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Registry

def RetiredAt (s : State) (slot : SlotId) : Prop :=
  ∃ h : slot < s.slots.length, s.slots.get ⟨slot, h⟩ = .retired

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
