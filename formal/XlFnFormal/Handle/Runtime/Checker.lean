import XlFnFormal.Handle.Runtime.Transition
import XlFnFormal.Handle.Registry.Checker

set_option autoImplicit false
set_option linter.unusedVariables false
set_option linter.unusedSimpArgs false

namespace XlFnFormal.Handle.Runtime

open Registry (SessionId SlotId Generation Token SlotState closeSlot maxGeneration nextGeneration?)

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
      if s.phase = .«open» then
        match s.findInitializer? id with
        | some { id := _, stage := .beforeInsert } =>
            match Registry.apply? s.registry .insertFresh with
            | some reg' =>
                some { s with
                  registry := reg'
                  initializers := s.updateInitializer id (.pending { session := s.registry.session, slot := s.registry.slots.length, generation := 1 }) }
            | none => none
        | _ => none
      else
        none

  | .insertPendingReuse id slotId gen =>
      if s.phase = .«open» then
        match s.findInitializer? id with
        | some { id := _, stage := .beforeInsert } =>
            match Registry.apply? s.registry (.insertReuse slotId gen) with
            | some reg' =>
                some { s with
                  registry := reg'
                  initializers := s.updateInitializer id (.pending { session := s.registry.session, slot := slotId, generation := gen }) }
            | none => none
        | _ => none
      else
        none

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
          match Registry.apply? s.registry (.removeReuse token nextGen) with
          | some reg' =>
              some { s with
                registry := reg'
                initializers := s.updateInitializer id .resolved }
          | none => none
      | _ => none

  | .rollbackPendingRetire id =>
      match s.findInitializer? id with
      | some { id := _, stage := .pending token } =>
          match Registry.apply? s.registry (.removeRetire token) with
          | some reg' =>
              some { s with
                registry := reg'
                initializers := s.updateInitializer id .resolved }
          | none => none
      | _ => none

  | .beginLookup token =>
      match Registry.apply? s.registry (.beginLookup token) with
      | some reg' => some { s with registry := reg' }
      | none => none

  | .endLookup =>
      match Registry.apply? s.registry .endLookup with
      | some reg' => some { s with registry := reg' }
      | none => none

  | .sealTopics =>
      if s.phase = .«open» then
        some { s with phase := .drainingPrepares }
      else
        none

  | .closeRegistry =>
      if s.phase = .drainingPrepares ∧ s.initializers = [] ∧ s.activePrepares = 0 then
        match Registry.apply? s.registry .closeRegistry with
        | some reg' =>
            some { s with
              phase := .registryClosed
              registry := reg' }
        | none => none
      else
        none

  | .finishClose =>
      if s.phase = .registryClosed then
        match Registry.apply? s.registry .finishClose with
        | some reg' =>
            some { s with phase := .closed }
        | none => none
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
      · rename_i hPhase
        split at h
        · rename_i init_id hFind
          split at h
          · rename_i reg' hRegApply
            cases h
            have hId : init_id = id := by have hEq := List.find?_some hFind; dsimp at hEq; exact beq_iff_eq.mp hEq
            subst hId
            exact Step.insertPendingFresh hPhase hFind (Registry.apply?_sound hRegApply)
          · contradiction
        · contradiction
      · contradiction
  | insertPendingReuse id slotId gen =>
      dsimp [apply?] at h
      split at h
      · rename_i hPhase
        split at h
        · rename_i init_id hFind
          split at h
          · rename_i reg' hRegApply
            cases h
            have hId : init_id = id := by have hEq := List.find?_some hFind; dsimp at hEq; exact beq_iff_eq.mp hEq
            subst hId
            exact Step.insertPendingReuse hPhase hFind (Registry.apply?_sound hRegApply)
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
          subst hId
          exact Step.publishTopic hPhase hFind
        · contradiction
      · contradiction
  | rollbackPendingReuse id nextGen =>
      dsimp [apply?] at h
      split at h
      · rename_i init_id token hFind
        split at h
        · rename_i reg' hRegApply
          cases h
          have hId : init_id = id := by have hEq := List.find?_some hFind; dsimp at hEq; exact beq_iff_eq.mp hEq
          subst hId
          exact Step.rollbackPendingReuse hFind (Registry.apply?_sound hRegApply)
        · contradiction
      · contradiction
  | rollbackPendingRetire id =>
      dsimp [apply?] at h
      split at h
      · rename_i init_id token hFind
        split at h
        · rename_i reg' hRegApply
          cases h
          have hId : init_id = id := by have hEq := List.find?_some hFind; dsimp at hEq; exact beq_iff_eq.mp hEq
          subst hId
          exact Step.rollbackPendingRetire hFind (Registry.apply?_sound hRegApply)
        · contradiction
      · contradiction
  | beginLookup token =>
      dsimp [apply?] at h
      split at h
      · rename_i reg' hRegApply
        cases h
        exact Step.beginLookup (Registry.apply?_sound hRegApply)
      · contradiction
  | endLookup =>
      dsimp [apply?] at h
      split at h
      · rename_i reg' hRegApply
        cases h
        exact Step.endLookup (Registry.apply?_sound hRegApply)
      · contradiction
  | sealTopics =>
      dsimp [apply?] at h
      by_cases hP : s.phase = .«open»
      · rw [if_pos hP] at h; cases h; exact Step.sealTopics hP
      · rw [if_neg hP] at h; contradiction
  | closeRegistry =>
      dsimp [apply?] at h
      split at h
      · rename_i hCond
        split at h
        · rename_i reg' hRegApply
          cases h
          exact Step.closeRegistry hCond.1 hCond.2.1 hCond.2.2 (Registry.apply?_sound hRegApply)
        · contradiction
      · contradiction
  | finishClose =>
      dsimp [apply?] at h
      split at h
      · rename_i hPhase
        split at h
        · rename_i reg' hRegApply
          cases h
          exact Step.finishClose hPhase (Registry.apply?_sound hRegApply)
        · contradiction
      · contradiction

theorem apply?_complete {s s' : State} {e : Event} (h : Step s e s') : apply? s e = some s' := by
  cases h with
  | beginPrepare hPhase => simp [apply?, hPhase]
  | endPrepare hPrep => simp [apply?, hPrep]
  | beginInitialize hPhase hPrep hFresh => simp [apply?, hPhase, hPrep, hFresh]
  | finishInitialize hFind hStage => cases hStage <;> simp [apply?, hFind, *]
  | insertPendingFresh hPhase hFind hRegStep =>
      dsimp [apply?]
      rw [if_pos hPhase, hFind]; dsimp
      rw [Registry.apply?_complete hRegStep]
  | insertPendingReuse hPhase hFind hRegStep =>
      dsimp [apply?]
      rw [if_pos hPhase, hFind]; dsimp
      rw [Registry.apply?_complete hRegStep]
  | publishTopic hPhase hFind => simp [apply?, hPhase, hFind]
  | rollbackPendingReuse hFind hRegStep =>
      dsimp [apply?]
      rw [hFind]; dsimp
      rw [Registry.apply?_complete hRegStep]
  | rollbackPendingRetire hFind hRegStep =>
      dsimp [apply?]
      rw [hFind]; dsimp
      rw [Registry.apply?_complete hRegStep]
  | beginLookup hRegStep =>
      dsimp [apply?]
      rw [Registry.apply?_complete hRegStep]
  | endLookup hRegStep =>
      dsimp [apply?]
      rw [Registry.apply?_complete hRegStep]
  | sealTopics hPhase => simp [apply?, hPhase]
  | closeRegistry hPhase hNoInits hNoPrepares hRegStep =>
      dsimp [apply?]
      have hC : s.phase = .drainingPrepares ∧ s.initializers = [] ∧ s.activePrepares = 0 := ⟨hPhase, hNoInits, hNoPrepares⟩
      rw [if_pos hC]
      rw [Registry.apply?_complete hRegStep]
  | finishClose hPhase hRegStep =>
      dsimp [apply?]
      rw [if_pos hPhase]
      rw [Registry.apply?_complete hRegStep]

end XlFnFormal.Handle.Runtime
