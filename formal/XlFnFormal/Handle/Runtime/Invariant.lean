import XlFnFormal.Handle.Runtime.Transition

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Runtime

open Registry (SessionId SlotId Generation Token SlotState closeSlot maxGeneration nextGeneration?)

def PendingRootsValid (s : State) : Prop :=
  ∀ init ∈ s.initializers,
    match init.stage with
    | .pending token => TokenLive s.registry token
    | _ => True

def InitializerIdsUnique (s : State) : Prop :=
  s.initializers.Pairwise (fun lhs rhs => lhs.id ≠ rhs.id)

def OperationInvariant (s : State) : Prop :=
  s.initializers.length ≤ s.activePrepares

def PhaseInvariant (s : State) : Prop :=
  match s.phase with
  | .«open» => s.registry.closed = false
  | .drainingPrepares => s.registry.closed = false
  | .registryClosed => s.registry.closed = true
  | .closed => s.registry.closed = true

def RuntimeInvariant (s : State) : Prop :=
  PhaseInvariant s ∧ OperationInvariant s ∧ InitializerIdsUnique s ∧ PendingRootsValid s

theorem initial_runtimeInvariant (session : SessionId) :
    RuntimeInvariant (initialState session) := by
  refine ⟨by rfl, Nat.le_refl 0, List.Pairwise.nil, ?_⟩
  intro init hIn
  contradiction

theorem Step.operationInvariant_preserved
    {s s' : State} {e : Event}
    (hInv : OperationInvariant s)
    (hStep : Step s e s') :
    OperationInvariant s' := by
  cases hStep with
  | beginPrepare => exact Nat.le_trans hInv (Nat.le_add_right _ _)
  | endPrepare hPrep => exact Nat.le_pred_of_lt hPrep
  | beginInitialize hPhase hPrep hFresh =>
      dsimp [OperationInvariant]
      rw [List.length_append]
      dsimp
      exact hPrep
  | finishInitialize hFind hStage =>
      rename_i id init
      dsimp [OperationInvariant, State.removeInitializer]
      have hLen : (s.initializers.filter (fun i => i.id != id)).length ≤ s.initializers.length := List.length_filter_le (fun i => i.id != id) s.initializers
      exact Nat.le_trans hLen hInv
  | insertPendingFresh hPhase hFind hReg =>
      dsimp [OperationInvariant, State.updateInitializer]
      rw [List.length_map]
      exact hInv
  | insertPendingReuse hPhase hFind hReg =>
      dsimp [OperationInvariant, State.updateInitializer]
      rw [List.length_map]
      exact hInv
  | publishTopic hPhase hFind =>
      dsimp [OperationInvariant, State.updateInitializer]
      rw [List.length_map]
      exact hInv
  | rollbackPendingReuse hFind hReg =>
      dsimp [OperationInvariant, State.updateInitializer]
      rw [List.length_map]
      exact hInv
  | rollbackPendingRetire hFind hReg =>
      dsimp [OperationInvariant, State.updateInitializer]
      rw [List.length_map]
      exact hInv
  | beginLookup => exact hInv
  | endLookup => exact hInv
  | sealTopics => exact hInv
  | closeRegistry => exact hInv
  | finishClose => exact hInv

theorem Reachable.operationInvariant_preserved
    {s t : State}
    (hInv : OperationInvariant s)
    (hReach : Reachable s t) :
    OperationInvariant t := by
  induction hReach with
  | refl => exact hInv
  | tail _ hStep ih => exact Step.operationInvariant_preserved ih hStep

end XlFnFormal.Handle.Runtime
