import XlFnFormal.Handle.Transition

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle

def apply? (s : State) (e : Event) : Option State :=
  match e with
  | .beginPrepare =>
      if s.phase = .«open» ∨ s.phase = .drainingPrepares then
        some { s with activePrepares := s.activePrepares + 1 }
      else
        none

  | .endPrepare =>
      if s.activePrepares > 0 then
        some { s with activePrepares := s.activePrepares - 1 }
      else
        none

  | .beginInitialize =>
      if (s.phase = .«open» ∨ s.phase = .drainingPrepares) ∧ s.activePrepares > 0 then
        some { s with activeInitializers := s.activeInitializers + 1 }
      else
        none

  | .finishInitialize =>
      if s.activeInitializers > 0 then
        some { s with activeInitializers := s.activeInitializers - 1 }
      else
        none

  | .publishTopic =>
      if s.phase = .«open» ∧ s.activeInitializers > 0 then
        some s
      else
        none

  | .rollbackPending =>
      if s.activeInitializers > 0 then
        some s
      else
        none

  | .insertFresh =>
      if s.MayInsert then
        some { s with slots := s.slots ++ [.live 1] }
      else
        none

  | .insertReuse slotId gen =>
      if hPre : s.MayInsert ∧ slotId < s.slots.length then
        if hVacant : s.slots.get ⟨slotId, hPre.2⟩ = .vacant gen then
          some { s with slots := s.slots.set slotId (.live gen) }
        else
          none
      else
        none

  | .removeReuse token nextGen =>
      if hPre : s.AuthenticatedFor token ∧ token.slot < s.slots.length ∧ nextGeneration? token.generation = some nextGen then
        if hLive : s.slots.get ⟨token.slot, hPre.2.1⟩ = .live token.generation then
          some { s with slots := s.slots.set token.slot (.vacant nextGen) }
        else
          none
      else
        none

  | .removeRetire token =>
      if hPre : s.AuthenticatedFor token ∧ token.slot < s.slots.length ∧ nextGeneration? token.generation = none then
        if hLive : s.slots.get ⟨token.slot, hPre.2.1⟩ = .live token.generation then
          some { s with slots := s.slots.set token.slot .retired }
        else
          none
      else
        none

  | .beginLookup token =>
      if hPre : s.phase ≠ .closed ∧ s.AuthenticatedFor token ∧ token.slot < s.slots.length then
        if hLive : s.slots.get ⟨token.slot, hPre.2.2⟩ = .live token.generation then
          some { s with activeLeases := s.activeLeases + 1 }
        else
          none
      else
        none

  | .endLookup =>
      if s.activeLeases > 0 then
        some { s with activeLeases := s.activeLeases - 1 }
      else
        none

  | .sealTopics =>
      if s.phase = .«open» then
        some { s with phase := .drainingPrepares }
      else
        none

  | .closeRegistry =>
      if (s.phase = .«open» ∨ s.phase = .drainingPrepares) ∧ s.activeInitializers = 0 ∧ s.activePrepares = 0 then
        some { s with
          phase := .registryClosed
          slots := s.slots.map closeSlot }
      else
        none

  | .finishClose =>
      if s.phase = .registryClosed ∧ s.activeLeases = 0 then
        some { s with phase := .closed }
      else
        none

theorem apply?_sound {s s' : State} {e : Event} (h : apply? s e = some s') : Step s e s' := by
  cases e with
  | beginPrepare =>
      by_cases hPre : s.phase = .«open» ∨ s.phase = .drainingPrepares
      · simp only [apply?] at h
        rw [if_pos hPre] at h
        cases h
        exact Step.beginPrepare hPre
      · simp only [apply?] at h
        rw [if_neg hPre] at h
        cases h
  | endPrepare =>
      by_cases hPre : s.activePrepares > 0
      · simp only [apply?] at h
        rw [if_pos hPre] at h
        cases h
        exact Step.endPrepare hPre
      · simp only [apply?] at h
        rw [if_neg hPre] at h
        cases h
  | beginInitialize =>
      by_cases hPre : (s.phase = .«open» ∨ s.phase = .drainingPrepares) ∧ s.activePrepares > 0
      · simp only [apply?] at h
        rw [if_pos hPre] at h
        cases h
        exact Step.beginInitialize hPre.1 hPre.2
      · simp only [apply?] at h
        rw [if_neg hPre] at h
        cases h
  | finishInitialize =>
      by_cases hPre : s.activeInitializers > 0
      · simp only [apply?] at h
        rw [if_pos hPre] at h
        cases h
        exact Step.finishInitialize hPre
      · simp only [apply?] at h
        rw [if_neg hPre] at h
        cases h
  | publishTopic =>
      by_cases hPre : s.phase = .«open» ∧ s.activeInitializers > 0
      · simp only [apply?] at h
        rw [if_pos hPre] at h
        cases h
        exact Step.publishTopic hPre.1 hPre.2
      · simp only [apply?] at h
        rw [if_neg hPre] at h
        cases h
  | rollbackPending =>
      by_cases hPre : s.activeInitializers > 0
      · simp only [apply?] at h
        rw [if_pos hPre] at h
        cases h
        exact Step.rollbackPending hPre
      · simp only [apply?] at h
        rw [if_neg hPre] at h
        cases h
  | insertFresh =>
      by_cases hPre : s.MayInsert
      · simp only [apply?] at h
        rw [if_pos hPre] at h
        cases h
        exact Step.insertFresh hPre
      · simp only [apply?] at h
        rw [if_neg hPre] at h
        cases h
  | insertReuse slotId gen =>
      by_cases hPre : s.MayInsert ∧ slotId < s.slots.length
      · by_cases hVacant : s.slots.get ⟨slotId, hPre.2⟩ = .vacant gen
        · simp only [apply?] at h
          rw [dif_pos hPre, dif_pos hVacant] at h
          cases h
          exact Step.insertReuse hPre.1 hPre.2 hVacant
        · simp only [apply?] at h
          rw [dif_pos hPre, dif_neg hVacant] at h
          cases h
      · simp only [apply?] at h
        rw [dif_neg hPre] at h
        cases h
  | removeReuse token nextGen =>
      by_cases hPre : s.AuthenticatedFor token ∧ token.slot < s.slots.length ∧ nextGeneration? token.generation = some nextGen
      · by_cases hLive : s.slots.get ⟨token.slot, hPre.2.1⟩ = .live token.generation
        · simp only [apply?] at h
          rw [dif_pos hPre, dif_pos hLive] at h
          cases h
          exact Step.removeReuse hPre.1 hPre.2.1 hLive hPre.2.2
        · simp only [apply?] at h
          rw [dif_pos hPre, dif_neg hLive] at h
          cases h
      · simp only [apply?] at h
        rw [dif_neg hPre] at h
        cases h
  | removeRetire token =>
      by_cases hPre : s.AuthenticatedFor token ∧ token.slot < s.slots.length ∧ nextGeneration? token.generation = none
      · by_cases hLive : s.slots.get ⟨token.slot, hPre.2.1⟩ = .live token.generation
        · simp only [apply?] at h
          rw [dif_pos hPre, dif_pos hLive] at h
          cases h
          exact Step.removeRetire hPre.1 hPre.2.1 hLive hPre.2.2
        · simp only [apply?] at h
          rw [dif_pos hPre, dif_neg hLive] at h
          cases h
      · simp only [apply?] at h
        rw [dif_neg hPre] at h
        cases h
  | beginLookup token =>
      by_cases hPre : s.phase ≠ .closed ∧ s.AuthenticatedFor token ∧ token.slot < s.slots.length
      · by_cases hLive : s.slots.get ⟨token.slot, hPre.2.2⟩ = .live token.generation
        · simp only [apply?] at h
          rw [dif_pos hPre, dif_pos hLive] at h
          cases h
          exact Step.beginLookup hPre.1 hPre.2.1 hPre.2.2 hLive
        · simp only [apply?] at h
          rw [dif_pos hPre, dif_neg hLive] at h
          cases h
      · simp only [apply?] at h
        rw [dif_neg hPre] at h
        cases h
  | endLookup =>
      by_cases hPre : s.activeLeases > 0
      · simp only [apply?] at h
        rw [if_pos hPre] at h
        cases h
        exact Step.endLookup hPre
      · simp only [apply?] at h
        rw [if_neg hPre] at h
        cases h
  | sealTopics =>
      by_cases hPre : s.phase = .«open»
      · simp only [apply?] at h
        rw [if_pos hPre] at h
        cases h
        exact Step.sealTopics hPre
      · simp only [apply?] at h
        rw [if_neg hPre] at h
        cases h
  | closeRegistry =>
      by_cases hPre : (s.phase = .«open» ∨ s.phase = .drainingPrepares) ∧ s.activeInitializers = 0 ∧ s.activePrepares = 0
      · simp only [apply?] at h
        rw [if_pos hPre] at h
        cases h
        exact Step.closeRegistry hPre.1 hPre.2.1 hPre.2.2
      · simp only [apply?] at h
        rw [if_neg hPre] at h
        cases h
  | finishClose =>
      by_cases hPre : s.phase = .registryClosed ∧ s.activeLeases = 0
      · simp only [apply?] at h
        rw [if_pos hPre] at h
        cases h
        exact Step.finishClose hPre.1 hPre.2
      · simp only [apply?] at h
        rw [if_neg hPre] at h
        cases h

theorem apply?_complete {s s' : State} {e : Event} (h : Step s e s') : apply? s e = some s' := by
  cases h <;> simp_all [apply?] <;> (try intro _ _; rfl)

end XlFnFormal.Handle
