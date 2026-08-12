import XlFnFormal.Handle.Transition

set_option autoImplicit false
set_option linter.unusedVariables false
set_option linter.unusedSimpArgs false

namespace XlFnFormal.Handle

def apply? (s : State) (e : Event) : Option State :=
  match e with
  | .beginPrepare =>
      if s.phase = .«open» ∨ s.phase = .drainingPrepares then
        some { s with activePrepares := s.activePrepares + 1 }
      else
        none

  | .endPrepare =>
      if s.activePrepares > s.initializers.length then
        some { s with activePrepares := s.activePrepares - 1 }
      else
        none

  | .beginInitialize id =>
      if s.phase = .«open» ∧ s.activePrepares > s.initializers.length ∧ s.findInitializer? id = none then
        some { s with initializers := s.initializers ++ [{ id := id, stage := .beforeInsert }] }
      else
        none

  | .finishInitialize id =>
      match s.findInitializer? id with
      | some init =>
          if init.stage = .beforeInsert ∨ init.stage = .resolved then
            some { s with initializers := s.removeInitializer id }
          else
            none
      | none => none

  | .insertPendingFresh id =>
      match s.findInitializer? id with
      | some { id := _, stage := .beforeInsert } =>
          some { s with
            slots := s.slots ++ [.live 1]
            initializers := s.updateInitializer id (.pending { session := s.session, slot := s.slots.length, generation := 1 }) }
      | _ => none

  | .insertPendingReuse id slotId gen =>
      match s.findInitializer? id with
      | some { id := _, stage := .beforeInsert } =>
          if hInBounds : slotId < s.slots.length then
            if s.slots.get ⟨slotId, hInBounds⟩ = .vacant gen then
              some { s with
                slots := s.slots.set slotId (.live gen)
                initializers := s.updateInitializer id (.pending { session := s.session, slot := slotId, generation := gen }) }
            else
              none
          else
            none
      | _ => none

  | .publishTopic id =>
      if s.phase = .«open» then
        match s.findInitializer? id with
        | some { id := _, stage := .pending token } =>
            some { s with initializers := s.updateInitializer id .resolved }
        | _ => none
      else
        none

  | .rollbackPendingReuse id nextGen =>
      match s.findInitializer? id with
      | some { id := _, stage := .pending token } =>
          if hInBounds : token.slot < s.slots.length then
            if s.slots.get ⟨token.slot, hInBounds⟩ = .live token.generation then
              if nextGeneration? token.generation = some nextGen then
                some { s with
                  slots := s.slots.set token.slot (.vacant nextGen)
                  initializers := s.updateInitializer id .resolved }
              else
                none
            else
              none
          else
            none
      | _ => none

  | .rollbackPendingRetire id =>
      match s.findInitializer? id with
      | some { id := _, stage := .pending token } =>
          if hInBounds : token.slot < s.slots.length then
            if s.slots.get ⟨token.slot, hInBounds⟩ = .live token.generation then
              if nextGeneration? token.generation = none then
                some { s with
                  slots := s.slots.set token.slot .retired
                  initializers := s.updateInitializer id .resolved }
              else
                none
            else
              none
          else
            none
      | _ => none

  | .removeReuse token nextGen =>
      if hPre : s.AuthenticatedFor token ∧ token.slot < s.slots.length ∧ nextGeneration? token.generation = some nextGen then
        if s.slots.get ⟨token.slot, hPre.2.1⟩ = .live token.generation then
          some { s with slots := s.slots.set token.slot (.vacant nextGen) }
        else
          none
      else
        none

  | .removeRetire token =>
      if hPre : s.AuthenticatedFor token ∧ token.slot < s.slots.length ∧ nextGeneration? token.generation = none then
        if s.slots.get ⟨token.slot, hPre.2.1⟩ = .live token.generation then
          some { s with slots := s.slots.set token.slot .retired }
        else
          none
      else
        none

  | .beginLookup token =>
      if hPre : s.phase ≠ .closed ∧ s.AuthenticatedFor token ∧ token.slot < s.slots.length then
        if s.slots.get ⟨token.slot, hPre.2.2⟩ = .live token.generation then
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
      if (s.phase = .«open» ∨ s.phase = .drainingPrepares) ∧ s.initializers = [] ∧ s.activePrepares = 0 then
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
      dsimp [apply?] at h
      by_cases hP : s.phase = .«open» ∨ s.phase = .drainingPrepares
      · rw [if_pos hP] at h; cases h; exact Step.beginPrepare hP
      · rw [if_neg hP] at h; contradiction
  | endPrepare =>
      dsimp [apply?] at h
      by_cases hP : s.activePrepares > s.initializers.length
      · rw [if_pos hP] at h; cases h; exact Step.endPrepare hP
      · rw [if_neg hP] at h; contradiction
  | beginInitialize id =>
      dsimp [apply?] at h
      by_cases hP : s.phase = .«open» ∧ s.activePrepares > s.initializers.length ∧ s.findInitializer? id = none
      · rw [if_pos hP] at h; cases h; exact Step.beginInitialize hP.1 hP.2.1 hP.2.2
      · rw [if_neg hP] at h; contradiction
  | finishInitialize id =>
      dsimp [apply?] at h
      split at h
      · rename_i init hFind
        split at h
        · rename_i hStage
          cases h
          exact Step.finishInitialize hFind hStage
        · contradiction
      · contradiction
  | insertPendingFresh id =>
      dsimp [apply?] at h
      split at h
      · rename_i init_id hFind
        cases h
        have hId : init_id = id := by have hEq := List.find?_some hFind; dsimp at hEq; exact beq_iff_eq.mp hEq
        subst hId; exact Step.insertPendingFresh hFind
      · contradiction
  | insertPendingReuse id slotId gen =>
      dsimp [apply?] at h
      split at h
      · rename_i init_id hFind
        split at h
        · rename_i hInBounds
          split at h
          · rename_i hVacant
            cases h
            have hId : init_id = id := by have hEq := List.find?_some hFind; dsimp at hEq; exact beq_iff_eq.mp hEq
            subst hId; exact Step.insertPendingReuse hFind hInBounds hVacant
          · contradiction
        · contradiction
      · contradiction
  | publishTopic id =>
      dsimp [apply?] at h
      split at h
      · rename_i hPhase
        split at h
        · rename_i init_id token hFind
          cases h
          have hId : init_id = id := by have hEq := List.find?_some hFind; dsimp at hEq; exact beq_iff_eq.mp hEq
          subst hId; exact Step.publishTopic hPhase hFind
        · contradiction
      · contradiction
  | rollbackPendingReuse id nextGen =>
      dsimp [apply?] at h
      split at h
      · rename_i init_id token hFind
        split at h
        · rename_i hInBounds
          split at h
          · rename_i hLive
            split at h
            · rename_i hNextGen
              cases h
              have hId : init_id = id := by have hEq := List.find?_some hFind; dsimp at hEq; exact beq_iff_eq.mp hEq
              subst hId; exact Step.rollbackPendingReuse hFind hInBounds hLive hNextGen
            · contradiction
          · contradiction
        · contradiction
      · contradiction
  | rollbackPendingRetire id =>
      dsimp [apply?] at h
      split at h
      · rename_i init_id token hFind
        split at h
        · rename_i hInBounds
          split at h
          · rename_i hLive
            split at h
            · rename_i hExhausted
              cases h
              have hId : init_id = id := by have hEq := List.find?_some hFind; dsimp at hEq; exact beq_iff_eq.mp hEq
              subst hId; exact Step.rollbackPendingRetire hFind hInBounds hLive hExhausted
            · contradiction
          · contradiction
        · contradiction
      · contradiction
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
      by_cases hP : s.activeLeases > 0
      · rw [if_pos hP] at h; cases h; exact Step.endLookup hP
      · rw [if_neg hP] at h; contradiction
  | sealTopics =>
      dsimp [apply?] at h
      by_cases hP : s.phase = .«open»
      · rw [if_pos hP] at h; cases h; exact Step.sealTopics hP
      · rw [if_neg hP] at h; contradiction
  | closeRegistry =>
      dsimp [apply?] at h
      by_cases hP : (s.phase = .«open» ∨ s.phase = .drainingPrepares) ∧ s.initializers = [] ∧ s.activePrepares = 0
      · rw [if_pos hP] at h; cases h; exact Step.closeRegistry hP.1 hP.2.1 hP.2.2
      · rw [if_neg hP] at h; contradiction
  | finishClose =>
      dsimp [apply?] at h
      by_cases hP : s.phase = .registryClosed ∧ s.activeLeases = 0
      · rw [if_pos hP] at h; cases h; exact Step.finishClose hP.1 hP.2
      · rw [if_neg hP] at h; contradiction

theorem apply?_complete {s s' : State} {e : Event} (h : Step s e s') : apply? s e = some s' := by
  cases h with
  | beginPrepare hPhase => simp [apply?, hPhase]
  | endPrepare hPrep => simp [apply?, hPrep]
  | beginInitialize hPhase hPrep hFresh => simp [apply?, hPhase, hPrep, hFresh]
  | finishInitialize hFind hStage => cases hStage <;> simp [apply?, hFind, *]
  | insertPendingFresh hFind => simp [apply?, hFind]
  | insertPendingReuse hFind hInBounds hVacant =>
      rename_i id slotId gen
      change s.slots[slotId] = .vacant gen at hVacant
      dsimp [apply?]; rw [hFind]; dsimp; rw [dif_pos hInBounds]; rw [if_pos hVacant]
  | publishTopic hPhase hFind => simp [apply?, hPhase, hFind]
  | rollbackPendingReuse hFind hInBounds hLive hNextGen =>
      rename_i id token nextGen
      change s.slots[token.slot] = .live token.generation at hLive
      dsimp [apply?]; rw [hFind]; dsimp; rw [dif_pos hInBounds]; rw [if_pos hLive]; rw [if_pos hNextGen]
  | rollbackPendingRetire hFind hInBounds hLive hExhausted =>
      rename_i id token
      change s.slots[token.slot] = .live token.generation at hLive
      dsimp [apply?]; rw [hFind]; dsimp; rw [dif_pos hInBounds]; rw [if_pos hLive]; rw [if_pos hExhausted]
  | removeReuse hAuth hInBounds hLive hNextGen =>
      rename_i token nextGen
      have hP : s.AuthenticatedFor token ∧ token.slot < s.slots.length ∧ nextGeneration? token.generation = some nextGen := ⟨hAuth, hInBounds, hNextGen⟩
      change s.slots[token.slot] = .live token.generation at hLive
      dsimp [apply?]; rw [dif_pos hP]; rw [if_pos hLive]
  | removeRetire hAuth hInBounds hLive hExhausted =>
      rename_i token
      have hP : s.AuthenticatedFor token ∧ token.slot < s.slots.length ∧ nextGeneration? token.generation = none := ⟨hAuth, hInBounds, hExhausted⟩
      change s.slots[token.slot] = .live token.generation at hLive
      dsimp [apply?]; rw [dif_pos hP]; rw [if_pos hLive]
  | beginLookup hPhase hAuth hInBounds hLive =>
      rename_i token
      have hP : s.phase ≠ .closed ∧ s.AuthenticatedFor token ∧ token.slot < s.slots.length := ⟨hPhase, hAuth, hInBounds⟩
      change s.slots[token.slot] = .live token.generation at hLive
      dsimp [apply?]; rw [dif_pos hP]; rw [if_pos hLive]
  | endLookup hLease => simp [apply?, hLease]
  | sealTopics hPhase => simp [apply?, hPhase]
  | closeRegistry hPhase hNoInits hNoPrepares => simp [apply?, hPhase, hNoInits, hNoPrepares]
  | finishClose hPhase hNoLeases => simp [apply?, hPhase, hNoLeases]

end XlFnFormal.Handle
