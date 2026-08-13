import XlFnFormal.Handle.Runtime.Checker
import XlFnFormal.Handle.Runtime.Safety

set_option autoImplicit false
set_option linter.unusedVariables false
set_option maxHeartbeats 400000

namespace XlFnFormal.Handle.Runtime

open Registry (SessionId SlotId Generation Token SlotState closeSlot maxGeneration nextGeneration?)

/-- A 10-step trace demonstrating the draining race + rollback sequence completes to quiescent.
    open → beginPrepare → beginInitialize → insertPendingFresh → sealTopics
    → (publishTopic blocked) → rollbackPendingReuse → finishInitialize
    → endPrepare → closeRegistry → finishClose -/
theorem draining_race_rollback_completes_quiescent
    (session : SessionId)
    (hMax : 1 < maxGeneration) :
    let s0 := initialState session
    let token1 : Token := { session := session, slot := 0, generation := 1 }
    -- Step 0: beginPrepare
    let s1 := { s0 with activePrepares := 1 }
    -- Step 1: beginInitialize 1
    let s2 := { s1 with initializers := [{ id := 1, stage := .beforeInsert }] }
    -- Step 2: insertPendingFresh 1
    let s3 := { s2 with
                registry := { s2.registry with slots := [.live 1] },
                initializers := [{ id := 1, stage := .pending token1 }] }
    -- Step 3: sealTopics
    let s4 := { s3 with phase := .drainingPrepares }
    -- Step 4: rollbackPendingReuse 1 2
    let s7 := { s4 with
                registry := { s4.registry with slots := [.vacant 2] },
                initializers := [{ id := 1, stage := .resolved }] }
    -- Step 5: finishInitialize 1
    let s8 := { s7 with initializers := [] }
    -- Step 6: endPrepare
    let s9 := { s8 with activePrepares := 0 }
    -- Step 7: closeRegistry
    let s10 := { s9 with
                 phase := .registryClosed,
                 registry := { s9.registry with closed := true } }
    -- Step 8: finishClose
    let s11 := { s10 with phase := .closed }
    Step s0 .beginPrepare s1 ∧
    Step s1 (.beginInitialize 1) s2 ∧
    Step s2 (.insertPendingFresh 1) s3 ∧
    Step s3 .sealTopics s4 ∧
    (¬ ∃ s', Step s4 (.publishTopic 1) s') ∧
    Step s4 (.rollbackPendingReuse 1 2) s7 ∧
    Step s7 (.finishInitialize 1) s8 ∧
    Step s8 .endPrepare s9 ∧
    Step s9 .closeRegistry s10 ∧
    Step s10 .finishClose s11 := by
  intro s0 token1 s1 s2 s3 s4 s7 s8 s9 s10 s11
  refine ⟨?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_⟩
  -- Step 0: beginPrepare
  · exact Step.beginPrepare (by left; rfl)
  -- Step 1: beginInitialize 1
  · exact Step.beginInitialize (by rfl) (by dsimp [s1, s0, initialState]; decide) (by rfl)
  -- Step 2: insertPendingFresh 1
  · have hFind2 : s2.findInitializer? 1 = some { id := 1, stage := .beforeInsert } := by rfl
    have hRegStep2 : Registry.Step s2.registry .insertFresh { s2.registry with slots := [.live 1] } :=
      Registry.Step.insertFresh (by rfl)
    exact Step.insertPendingFresh (by rfl) hFind2 hRegStep2
  -- Step 3: sealTopics
  · exact Step.sealTopics (by rfl)
  -- publishTopic blocked after seal
  · exact no_topic_publication_after_seal (by rfl)
  -- Step 4: rollbackPendingReuse
  · have hNext : nextGeneration? 1 = some 2 := by dsimp [nextGeneration?]; rw [if_pos hMax]
    have hInBounds3 : token1.slot < s4.registry.slots.length := by
      dsimp [token1, s4, s3, s2, s1, s0, initialState]; decide
    have hLive3 : s4.registry.slots.get ⟨token1.slot, hInBounds3⟩ = .live token1.generation := by rfl
    have hAuth3 : s4.registry.AuthenticatedFor token1 := by rfl
    have hRegStep4 : Registry.Step s4.registry (.removeReuse token1 2) { s4.registry with slots := [.vacant 2] } :=
      Registry.Step.removeReuse hAuth3 hInBounds3 hLive3 hNext
    have hFind4 : s4.findInitializer? 1 = some { id := 1, stage := .pending token1 } := by rfl
    exact Step.rollbackPendingReuse hFind4 hRegStep4
  -- Step 5: finishInitialize
  · have hFind7 : s7.findInitializer? 1 = some { id := 1, stage := .resolved } := by rfl
    exact Step.finishInitialize hFind7 (by right; rfl)
  -- Step 6: endPrepare
  · exact Step.endPrepare (by dsimp [s8, s7, s4, s3, s2, s1, s0, initialState]; decide)
  -- Step 7: closeRegistry
  · have hRegStep9 : Registry.Step s9.registry .closeRegistry { s9.registry with closed := true } :=
      Registry.Step.closeRegistry (by rfl)
    exact Step.closeRegistry (by rfl) (by rfl) (by rfl) hRegStep9
  -- Step 8: finishClose
  · have hRegStep10 : Registry.Step s10.registry .finishClose s10.registry :=
      Registry.Step.finishClose (by rfl) (by rfl)
    exact Step.finishClose (by rfl) hRegStep10

end XlFnFormal.Handle.Runtime
