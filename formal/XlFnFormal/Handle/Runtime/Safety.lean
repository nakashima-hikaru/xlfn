import XlFnFormal.Handle.Registry.Safety
import XlFnFormal.Handle.Runtime.Invariant

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Runtime

open Registry (SessionId SlotId Generation Token SlotState closeSlot maxGeneration nextGeneration?)

theorem no_topic_publication_after_seal
    {s : State} {id : InitializerId} (hSealed : s.phase = .drainingPrepares) :
    ¬ ∃ s', Step s (.publishTopic id) s' := by
  intro ⟨s', hStep⟩
  cases hStep
  rename_i hPhase _
  rw [hSealed] at hPhase
  contradiction

theorem registry_close_waits_for_initializers
    {s s' : State} (hStep : Step s .closeRegistry s') :
    s.initializers = [] ∧ s.activePrepares = 0 := by
  cases hStep
  exact ⟨by assumption, by assumption⟩

-- Helper lemma: after updateInitializer, find? still finds the updated entry
-- Helper: the condition function is invariant under updateInitializer mapping
private theorem updateInitializer_pred_invariant
    {i : Initializer} {id : InitializerId} {stage : InitializerStage} :
    ((if i.id == id then { i with stage := stage } else i).id == id) = (i.id == id) := by
  cases h : i.id == id <;> simp [h]

theorem updateInitializer_find
    {inits : List Initializer} {id : InitializerId} {token : Token} {stage : InitializerStage}
    (hFind : inits.find? (fun i => i.id == id) = some { id := id, stage := .pending token }) :
    (inits.map (fun i => if i.id == id then { i with stage := stage } else i)).find? (fun i => i.id == id) =
      some { id := id, stage := stage } := by
  induction inits with
  | nil => contradiction
  | cons i rest ih =>
      simp only [List.map, List.find?]
      rw [updateInitializer_pred_invariant]
      by_cases hEq : (i.id == id) = true
      · simp only [hEq]
        simp only [List.find?, hEq] at hFind
        cases hFind
        rfl
      · have hF : (i.id == id) = false := Bool.not_eq_true _ |>.mp hEq
        simp only [hF]
        simp only [List.find?, hF] at hFind
        exact ih hFind

theorem rollback_removes_pending_root_reuse
    {s s' : State} {id : InitializerId} {token : Token} {nextGen : Generation}
    (hFind : s.findInitializer? id = some { id := id, stage := .pending token })
    (hStep : Step s (.rollbackPendingReuse id nextGen) s') :
    s'.findInitializer? id = some { id := id, stage := .resolved } ∧ ¬ TokenLive s'.registry token := by
  cases hStep with
  | rollbackPendingReuse hFind' hRegStep =>
      refine ⟨by dsimp [State.findInitializer?, State.updateInitializer]; exact updateInitializer_find hFind', ?_⟩
      intro hLiveTok
      rcases hLiveTok with ⟨hSess, ⟨hBounds, hLive⟩⟩
      rw [hFind] at hFind'
      cases hFind'
      cases hRegStep
      dsimp at hLive
      rw [List.getElem_set_self] at hLive
      contradiction

theorem rollback_removes_pending_root_retire
    {s s' : State} {id : InitializerId} {token : Token}
    (hFind : s.findInitializer? id = some { id := id, stage := .pending token })
    (hStep : Step s (.rollbackPendingRetire id) s') :
    s'.findInitializer? id = some { id := id, stage := .resolved } ∧ ¬ TokenLive s'.registry token := by
  cases hStep with
  | rollbackPendingRetire hFind' hRegStep =>
      refine ⟨by dsimp [State.findInitializer?, State.updateInitializer]; exact updateInitializer_find hFind', ?_⟩
      intro hLiveTok
      rcases hLiveTok with ⟨hSess, ⟨hBounds, hLive⟩⟩
      rw [hFind] at hFind'
      cases hFind'
      cases hRegStep
      dsimp at hLive
      rw [List.getElem_set_self] at hLive
      contradiction

def CloseCertified (s : State) : Prop :=
  s.phase = .closed ∧
  Registry.CloseCertified s.registry

theorem Step.closeCertified_of_finishClose
    {s s' : State}
    (hStep : Step s .finishClose s') :
    CloseCertified s' := by
  cases hStep with
  | finishClose hPhase hRegStep =>
      cases hRegStep with
      | finishClose hClosed hLeases =>
          exact ⟨rfl, ⟨hClosed, hLeases⟩⟩

theorem draining_pending_insert_cannot_escape
    {s : State} {id : InitializerId} {token : Token}
    (hSealed : s.phase = .drainingPrepares)
    (hFind : s.findInitializer? id = some { id := id, stage := .pending token })
    (hInBounds : token.slot < s.registry.slots.length)
    (hLive : s.registry.slots.get ⟨token.slot, hInBounds⟩ = .live token.generation)
    (hAuth : s.registry.AuthenticatedFor token) :
    (¬ ∃ s', Step s (.publishTopic id) s') ∧
    ((∃ nextGen s', Step s (.rollbackPendingReuse id nextGen) s' ∧ s'.findInitializer? id = some { id := id, stage := .resolved }) ∨
     (∃ s', Step s (.rollbackPendingRetire id) s' ∧ s'.findInitializer? id = some { id := id, stage := .resolved })) := by
  refine ⟨no_topic_publication_after_seal hSealed, ?_⟩
  cases hNext : nextGeneration? token.generation with
  | some nextGen =>
      left
      have hRegStep : Registry.Step s.registry (.removeReuse token nextGen) { s.registry with slots := s.registry.slots.set token.slot (.vacant nextGen) } :=
        Registry.Step.removeReuse hAuth hInBounds hLive hNext
      have hStep : Step s (.rollbackPendingReuse id nextGen) { s with registry := { s.registry with slots := s.registry.slots.set token.slot (.vacant nextGen) }, initializers := s.updateInitializer id .resolved } :=
        Step.rollbackPendingReuse hFind hRegStep
      refine ⟨nextGen, _, hStep, (rollback_removes_pending_root_reuse hFind hStep).1⟩
  | none =>
      right
      have hRegStep : Registry.Step s.registry (.removeRetire token) { s.registry with slots := s.registry.slots.set token.slot .retired } :=
        Registry.Step.removeRetire hAuth hInBounds hLive hNext
      have hStep : Step s (.rollbackPendingRetire id) { s with registry := { s.registry with slots := s.registry.slots.set token.slot .retired }, initializers := s.updateInitializer id .resolved } :=
        Step.rollbackPendingRetire hFind hRegStep
      refine ⟨_, hStep, (rollback_removes_pending_root_retire hFind hStep).1⟩

end XlFnFormal.Handle.Runtime
