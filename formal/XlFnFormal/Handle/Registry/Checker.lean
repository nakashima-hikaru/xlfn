import XlFnFormal.Handle.Registry.Transition

set_option autoImplicit false
set_option linter.unusedVariables false
set_option linter.unusedSimpArgs false

namespace XlFnFormal.Handle.Registry

def apply? (s : State) (e : Event) : Option State :=
  match e with
  | .insertFresh =>
      if s.closed = false then
        some { s with slots := s.slots ++ [.live 1] }
      else
        none

  | .insertReuse slotId gen =>
      if s.closed = false then
        if hInBounds : slotId < s.slots.length then
          if s.slots.get ⟨slotId, hInBounds⟩ = .vacant gen then
            some { s with slots := s.slots.set slotId (.live gen) }
          else
            none
        else
          none
      else
        none

  | .removeReuse token nextGen =>
      if hPre : token.session = s.session ∧ token.slot < s.slots.length ∧ nextGeneration? token.generation = some nextGen then
        if s.slots.get ⟨token.slot, hPre.2.1⟩ = .live token.generation then
          some { s with slots := s.slots.set token.slot (.vacant nextGen) }
        else
          none
      else
        none

  | .removeRetire token =>
      if hPre : token.session = s.session ∧ token.slot < s.slots.length ∧ nextGeneration? token.generation = none then
        if s.slots.get ⟨token.slot, hPre.2.1⟩ = .live token.generation then
          some { s with slots := s.slots.set token.slot .retired }
        else
          none
      else
        none

  | .beginLookup token =>
      if hPre : s.closed = false ∧ token.session = s.session ∧ token.slot < s.slots.length then
        if s.slots.get ⟨token.slot, hPre.2.2⟩ = .live token.generation then
          some { s with activeBorrows := s.activeBorrows + 1 }
        else
          none
      else
        none

  | .endLookup =>
      if s.activeBorrows > 0 then
        some { s with activeBorrows := s.activeBorrows - 1 }
      else
        none

  | .closeRegistry =>
      if s.closed = false then
        some { s with
          closed := true
          slots := s.slots.map closeSlot }
      else
        none

  | .finishClose =>
      if s.closed ∧ s.activeBorrows = 0 then
        some s
      else
        none

theorem apply?_sound {s s' : State} {e : Event} (h : apply? s e = some s') : Step s e s' := by
  cases e with
  | insertFresh =>
      dsimp [apply?] at h
      by_cases hMay : s.closed = false
      · rw [if_pos hMay] at h; cases h; exact Step.insertFresh hMay
      · rw [if_neg hMay] at h; contradiction
  | insertReuse slotId gen =>
      dsimp [apply?] at h
      by_cases hMay : s.closed = false
      · rw [if_pos hMay] at h
        split at h
        · rename_i hInBounds
          split at h
          · rename_i hVacant
            cases h
            exact Step.insertReuse hMay hInBounds hVacant
          · contradiction
        · contradiction
      · rw [if_neg hMay] at h; contradiction
  | removeReuse token nextGen =>
      dsimp [apply?] at h
      split at h
      · rename_i hPre
        split at h
        · cases h
          rename_i hLive
          exact Step.removeReuse hPre.1 hPre.2.1 hLive hPre.2.2
        · contradiction
      · contradiction
  | removeRetire token =>
      dsimp [apply?] at h
      split at h
      · rename_i hPre
        split at h
        · cases h
          rename_i hLive
          exact Step.removeRetire hPre.1 hPre.2.1 hLive hPre.2.2
        · contradiction
      · contradiction
  | beginLookup token =>
      dsimp [apply?] at h
      split at h
      · rename_i hPre
        split at h
        · cases h
          rename_i hLive
          exact Step.beginLookup hPre.1 hPre.2.1 hPre.2.2 hLive
        · contradiction
      · contradiction
  | endLookup =>
      dsimp [apply?] at h
      by_cases hP : s.activeBorrows > 0
      · rw [if_pos hP] at h; cases h; exact Step.endLookup hP
      · rw [if_neg hP] at h; contradiction
  | closeRegistry =>
      dsimp [apply?] at h
      by_cases hNotClosed : s.closed = false
      · rw [if_pos hNotClosed] at h; cases h; exact Step.closeRegistry hNotClosed
      · rw [if_neg hNotClosed] at h; contradiction
  | finishClose =>
      dsimp [apply?] at h
      by_cases hP : s.closed ∧ s.activeBorrows = 0
      · rw [if_pos hP] at h; cases h; exact Step.finishClose hP.1 hP.2
      · rw [if_neg hP] at h; contradiction

theorem apply?_complete {s s' : State} {e : Event} (h : Step s e s') : apply? s e = some s' := by
  cases h with
  | insertFresh hMay =>
      have hMay' : s.closed = false := hMay
      dsimp [apply?]; rw [if_pos hMay']
  | insertReuse hMay hInBounds hVacant =>
      rename_i slotId gen
      have hMay' : s.closed = false := hMay
      change s.slots[slotId] = .vacant gen at hVacant
      dsimp [apply?]; rw [if_pos hMay']; rw [dif_pos hInBounds]; rw [if_pos hVacant]
  | removeReuse hAuth hInBounds hLive hNextGen =>
      rename_i token nextGen
      have hP : token.session = s.session ∧ token.slot < s.slots.length ∧ nextGeneration? token.generation = some nextGen := ⟨hAuth, hInBounds, hNextGen⟩
      change s.slots[token.slot] = .live token.generation at hLive
      dsimp [apply?]; rw [dif_pos hP]; rw [if_pos hLive]
  | removeRetire hAuth hInBounds hLive hExhausted =>
      rename_i token
      have hP : token.session = s.session ∧ token.slot < s.slots.length ∧ nextGeneration? token.generation = none := ⟨hAuth, hInBounds, hExhausted⟩
      change s.slots[token.slot] = .live token.generation at hLive
      dsimp [apply?]; rw [dif_pos hP]; rw [if_pos hLive]
  | beginLookup hNotClosed hAuth hInBounds hLive =>
      rename_i token
      have hP : s.closed = false ∧ token.session = s.session ∧ token.slot < s.slots.length := ⟨hNotClosed, hAuth, hInBounds⟩
      change s.slots[token.slot] = .live token.generation at hLive
      dsimp [apply?]; rw [dif_pos hP]; rw [if_pos hLive]
  | endLookup hBorrows => simp [apply?, hBorrows]
  | closeRegistry hNotClosed =>
      dsimp [apply?]; rw [if_pos hNotClosed]
  | finishClose hClosed hNoBorrows => simp [apply?, hClosed, hNoBorrows]

end XlFnFormal.Handle.Registry
