import XlFnFormal.Handle.Runtime.Transition
import XlFnFormal.Handle.Registry.Invariant

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

def stageTokenNe (l r : InitializerStage) : Prop :=
  match l, r with
  | .pending lt, .pending rt => lt ≠ rt
  | _, _ => True

def PendingTokensUnique (s : State) : Prop :=
  s.initializers.Pairwise (fun lhs rhs => stageTokenNe lhs.stage rhs.stage)

def OperationInvariant (s : State) : Prop :=
  s.initializers.length ≤ s.activePrepares

def PhaseInvariant (s : State) : Prop :=
  match s.phase with
  | .«open» => s.registry.closed = false
  | .drainingPrepares => s.registry.closed = false
  | .registryClosed =>
      s.registry.closed = true ∧
      s.activePrepares = 0 ∧
      s.initializers = [] ∧
      Registry.NoLiveSlots s.registry
  | .closed =>
      s.registry.closed = true ∧
      s.activePrepares = 0 ∧
      s.initializers = [] ∧
      s.registry.activeLeases = 0 ∧
      Registry.NoLiveSlots s.registry

def RuntimeInvariant (s : State) : Prop :=
  PhaseInvariant s ∧
  OperationInvariant s ∧
  InitializerIdsUnique s ∧
  PendingTokensUnique s ∧
  PendingRootsValid s

theorem initial_runtimeInvariant (session : SessionId) :
    RuntimeInvariant (initialState session) := by
  refine ⟨by rfl, Nat.le_refl 0, List.Pairwise.nil, List.Pairwise.nil, ?_⟩
  intro init hIn
  contradiction

theorem phaseInvariant_registryClosed_fields
    {s : State}
    (hInv : PhaseInvariant s)
    (hPhase : s.phase = .registryClosed) :
    s.registry.closed = true ∧
    s.activePrepares = 0 ∧
    s.initializers = [] ∧
    Registry.NoLiveSlots s.registry := by
  dsimp [PhaseInvariant] at hInv
  rw [hPhase] at hInv
  exact hInv

theorem phaseInvariant_closed_fields
    {s : State}
    (hInv : PhaseInvariant s)
    (hPhase : s.phase = .closed) :
    s.registry.closed = true ∧
    s.activePrepares = 0 ∧
    s.initializers = [] ∧
    s.registry.activeLeases = 0 ∧
    Registry.NoLiveSlots s.registry := by
  dsimp [PhaseInvariant] at hInv
  rw [hPhase] at hInv
  exact hInv

theorem Step.phaseInvariant_preserved
    {s s' : State} {e : Event}
    (hInv : PhaseInvariant s)
    (hStep : Step s e s') :
    PhaseInvariant s' := by
  cases hStep with
  | beginPrepare hPhase =>
      dsimp [PhaseInvariant] at hInv ⊢
      cases hPhase with
      | inl hOpen => rw [hOpen] at hInv ⊢; exact hInv
      | inr hDraining => rw [hDraining] at hInv ⊢; exact hInv
  | endPrepare hPrep =>
      dsimp [PhaseInvariant] at hInv ⊢
      cases hP : s.phase with
      | «open» => simpa [hP, PhaseInvariant] using hInv
      | drainingPrepares => simpa [hP, PhaseInvariant] using hInv
      | registryClosed =>
          have hInv' := phaseInvariant_registryClosed_fields hInv hP
          rw [hInv'.2.1] at hPrep
          contradiction
      | closed =>
          have hInv' := phaseInvariant_closed_fields hInv hP
          rw [hInv'.2.1] at hPrep
          contradiction
  | beginInitialize hPhase hPrep hFresh =>
      dsimp [PhaseInvariant] at hInv ⊢
      rw [hPhase] at hInv ⊢
      exact hInv
  | finishInitialize hFind hStage =>
      rename_i id init
      dsimp [PhaseInvariant] at hInv ⊢
      cases hP : s.phase with
      | «open» => simpa [hP, PhaseInvariant] using hInv
      | drainingPrepares => simpa [hP, PhaseInvariant] using hInv
      | registryClosed =>
          have hInv' := phaseInvariant_registryClosed_fields hInv hP
          have hNil : s.findInitializer? id = none := by
            dsimp [State.findInitializer?]
            rw [hInv'.2.2.1]
            rfl
          rw [hNil] at hFind
          cases hFind
      | closed =>
          have hInv' := phaseInvariant_closed_fields hInv hP
          have hNil : s.findInitializer? id = none := by
            dsimp [State.findInitializer?]
            rw [hInv'.2.2.1]
            rfl
          rw [hNil] at hFind
          cases hFind
  | insertPendingFresh hPhase hFind hRegStep =>
      cases hRegStep
      dsimp [PhaseInvariant] at hInv ⊢
      cases hPhase with
      | inl hOpen => rw [hOpen] at hInv ⊢; exact hInv
      | inr hDraining => rw [hDraining] at hInv ⊢; exact hInv
  | insertPendingReuse hPhase hFind hRegStep =>
      cases hRegStep
      dsimp [PhaseInvariant] at hInv ⊢
      cases hPhase with
      | inl hOpen => rw [hOpen] at hInv ⊢; exact hInv
      | inr hDraining => rw [hDraining] at hInv ⊢; exact hInv
  | publishTopic hPhase hFind =>
      dsimp [PhaseInvariant] at hInv ⊢
      rw [hPhase] at hInv ⊢
      exact hInv
  | rollbackPendingReuse hFind hRegStep =>
      rename_i id token nextGen reg'
      cases hRegStep
      dsimp [PhaseInvariant] at hInv ⊢
      cases hP : s.phase with
      | «open» => simpa [hP, PhaseInvariant] using hInv
      | drainingPrepares => simpa [hP, PhaseInvariant] using hInv
      | registryClosed =>
          have hInv' := phaseInvariant_registryClosed_fields hInv hP
          have hNil : s.findInitializer? id = none := by
            dsimp [State.findInitializer?]
            rw [hInv'.2.2.1]
            rfl
          rw [hNil] at hFind
          cases hFind
      | closed =>
          have hInv' := phaseInvariant_closed_fields hInv hP
          have hNil : s.findInitializer? id = none := by
            dsimp [State.findInitializer?]
            rw [hInv'.2.2.1]
            rfl
          rw [hNil] at hFind
          cases hFind
  | rollbackPendingRetire hFind hRegStep =>
      rename_i id token reg'
      cases hRegStep
      dsimp [PhaseInvariant] at hInv ⊢
      cases hP : s.phase with
      | «open» => simpa [hP, PhaseInvariant] using hInv
      | drainingPrepares => simpa [hP, PhaseInvariant] using hInv
      | registryClosed =>
          have hInv' := phaseInvariant_registryClosed_fields hInv hP
          have hNil : s.findInitializer? id = none := by
            dsimp [State.findInitializer?]
            rw [hInv'.2.2.1]
            rfl
          rw [hNil] at hFind
          cases hFind
      | closed =>
          have hInv' := phaseInvariant_closed_fields hInv hP
          have hNil : s.findInitializer? id = none := by
            dsimp [State.findInitializer?]
            rw [hInv'.2.2.1]
            rfl
          rw [hNil] at hFind
          cases hFind
  | beginLookup hRegStep =>
      cases hRegStep with
      | beginLookup hNotClosed hAuth hInBounds hLive =>
          dsimp [PhaseInvariant] at hInv ⊢
          cases hP : s.phase with
          | «open» => simpa [hP, PhaseInvariant] using hInv
          | drainingPrepares => simpa [hP, PhaseInvariant] using hInv
          | registryClosed =>
              have hInv' := phaseInvariant_registryClosed_fields hInv hP
              rw [hInv'.1] at hNotClosed
              contradiction
          | closed =>
              have hInv' := phaseInvariant_closed_fields hInv hP
              rw [hInv'.1] at hNotClosed
              contradiction
  | endLookup hRegStep =>
      cases hRegStep with
      | endLookup hLeases =>
          dsimp [PhaseInvariant] at hInv ⊢
          cases hP : s.phase with
          | «open» => simpa [hP, PhaseInvariant] using hInv
          | drainingPrepares => simpa [hP, PhaseInvariant] using hInv
          | registryClosed => simpa [hP, PhaseInvariant, Registry.NoLiveSlots] using hInv
          | closed =>
              have hInv' := phaseInvariant_closed_fields hInv hP
              rw [hInv'.2.2.2.1] at hLeases
              contradiction
  | sealTopics hPhase =>
      dsimp [PhaseInvariant] at hInv ⊢
      rw [hPhase] at hInv
      exact hInv
  | closeRegistry hPhase hNoInits hNoPrepares hRegStep =>
      cases hRegStep
      dsimp [PhaseInvariant]
      exact ⟨by rfl, hNoPrepares, hNoInits,
        Registry.map_closeSlot_noLiveSlots s.registry⟩
  | finishClose hPhase hRegStep =>
      cases hRegStep with
      | finishClose hClosed hNoLeases =>
          dsimp [PhaseInvariant] at hInv ⊢
          have hInv' := phaseInvariant_registryClosed_fields hInv hPhase
          exact ⟨hClosed, hInv'.2.1, hInv'.2.2.1, hNoLeases, hInv'.2.2.2⟩

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
      have hLen : (s.initializers.filter (fun i => i.id != id)).length ≤ s.initializers.length := by
        induction s.initializers with
        | nil => exact Nat.le_refl 0
        | cons x xs ih =>
            dsimp [List.filter]
            split
            · dsimp; exact Nat.succ_le_succ ih
            · exact Nat.le_succ_of_le ih
      exact Nat.le_trans hLen hInv
  | insertPendingFresh =>
      dsimp [OperationInvariant, State.updateInitializer]
      rw [List.length_map]
      exact hInv
  | insertPendingReuse =>
      dsimp [OperationInvariant, State.updateInitializer]
      rw [List.length_map]
      exact hInv
  | publishTopic =>
      dsimp [OperationInvariant, State.updateInitializer]
      rw [List.length_map]
      exact hInv
  | rollbackPendingReuse =>
      dsimp [OperationInvariant, State.updateInitializer]
      rw [List.length_map]
      exact hInv
  | rollbackPendingRetire =>
      dsimp [OperationInvariant, State.updateInitializer]
      rw [List.length_map]
      exact hInv
  | beginLookup => exact hInv
  | endLookup => exact hInv
  | sealTopics => exact hInv
  | closeRegistry => exact hInv
  | finishClose => exact hInv

theorem List.mem_of_mem_filter' {α : Type} {p : α → Bool} {x : α} {l : List α}
    (h : x ∈ l.filter p) : x ∈ l := by
  induction l with
  | nil => contradiction
  | cons y ys ih =>
      dsimp [List.filter] at h
      split at h
      · cases List.mem_cons.mp h with
        | inl h1 => subst h1; exact List.mem_cons_self
        | inr h2 => exact List.mem_cons_of_mem y (ih h2)
      · exact List.mem_cons_of_mem y (ih h)

theorem List.mem_of_find?_eq_some' {α : Type} {p : α → Bool} {x : α} {l : List α}
    (h : l.find? p = some x) : x ∈ l := by
  induction l with
  | nil => contradiction
  | cons y ys ih =>
      dsimp [List.find?] at h
      split at h
      · cases h; exact List.mem_cons_self
      · exact List.mem_cons_of_mem y (ih h)

theorem List.Pairwise.mem_ne {α : Type} {R : α → α → Prop} {x y : α} {l : List α}
    (hP : l.Pairwise R) (hX : x ∈ l) (hY : y ∈ l) (hNe : x ≠ y) : R x y ∨ R y x := by
  induction hP with
  | nil => contradiction
  | cons hHead hTail ih =>
      cases List.mem_cons.mp hX with
      | inl hX1 =>
          subst hX1
          cases List.mem_cons.mp hY with
          | inl hY1 => subst hY1; contradiction
          | inr hY2 => left; exact hHead y hY2
      | inr hX2 =>
          cases List.mem_cons.mp hY with
          | inl hY1 =>
              subst hY1; right; exact hHead x hX2
          | inr hY2 =>
              exact ih hX2 hY2

-- Pairwise filter preservation helper
theorem List.Pairwise.filter {α : Type} {R : α → α → Prop} (p : α → Bool) {l : List α}
    (h : l.Pairwise R) : (l.filter p).Pairwise R := by
  induction h with
  | nil => exact List.Pairwise.nil
  | cons hHead hTail ih =>
      dsimp [List.filter]
      split
      · refine List.Pairwise.cons ?_ ih
        intro x hx
        exact hHead x (List.mem_of_mem_filter' hx)
      · exact ih

-- Pairwise map preservation helper when predicate is invariant
theorem List.Pairwise.map_update
    {inits : List Initializer} {id : InitializerId} {newStage : InitializerStage}
    {R : Initializer → Initializer → Prop}
    (hP_ids : inits.Pairwise (fun lhs rhs => lhs.id ≠ rhs.id))
    (hR : ∀ {a b : Initializer}, a ∈ inits → b ∈ inits → a.id ≠ b.id → R a b →
      R (if a.id == id then { a with stage := newStage } else a)
        (if b.id == id then { b with stage := newStage } else b))
    (hP : inits.Pairwise R) :
    (inits.map (fun i => if i.id == id then { i with stage := newStage } else i)).Pairwise R := by
  induction hP with
  | nil => exact List.Pairwise.nil
  | cons hHead hTail ih =>
      cases hP_ids with
      | cons hHeadIds hTailIds =>
          simp only [List.map]
          refine List.Pairwise.cons ?_ (ih hTailIds (fun {a b} hMemA hMemB hNeId hR' => hR (List.mem_cons_of_mem _ hMemA) (List.mem_cons_of_mem _ hMemB) hNeId hR'))
          intro x hx
          rcases List.mem_map.mp hx with ⟨y, hy, rfl⟩
          exact hR List.mem_cons_self (List.mem_cons_of_mem _ hy) (hHeadIds y hy) (hHead y hy)

theorem updateInitializer_id_ne
    {id : InitializerId} {newStage : InitializerStage} {a b : Initializer} (h : a.id ≠ b.id) :
    (if a.id == id then { a with stage := newStage } else a).id ≠
    (if b.id == id then { b with stage := newStage } else b).id := by
  by_cases hA : (a.id == id) = true <;> by_cases hB : (b.id == id) = true
  · have h1 : a.id = id := beq_iff_eq.mp hA
    have h2 : b.id = id := beq_iff_eq.mp hB
    have h3 : a.id = b.id := by rw [h1, h2]
    rw [h3] at h
    contradiction
  · have hB' : (b.id == id) = false := Bool.not_eq_true _ |>.mp hB
    simp [hA, hB']
    intro hEq
    have h1 : a.id = id := beq_iff_eq.mp hA
    have h2 : b.id = id := by rw [← hEq, h1]
    have h3 : (b.id == id) = true := beq_iff_eq.mpr h2
    rw [h3] at hB'
    contradiction
  · have hA' : (a.id == id) = false := Bool.not_eq_true _ |>.mp hA
    simp [hA', hB]
    intro hEq
    have h1 : b.id = id := beq_iff_eq.mp hB
    have h2 : a.id = id := by rw [hEq, h1]
    have h3 : (a.id == id) = true := beq_iff_eq.mpr h2
    rw [h3] at hA'
    contradiction
  · have hA' : (a.id == id) = false := Bool.not_eq_true _ |>.mp hA
    have hB' : (b.id == id) = false := Bool.not_eq_true _ |>.mp hB
    simp [hA', hB']
    exact h

theorem mem_map_update_id_ne
    {inits : List Initializer} {id : InitializerId} {newStage : InitializerStage} {b : Initializer}
    (hMem : b ∈ inits.map (fun i => if i.id == id then { i with stage := newStage } else i))
    (hNe : (b.id == id) = false) :
    b ∈ inits := by
  rcases List.mem_map.mp hMem with ⟨y, hy, rfl⟩
  by_cases hY : (y.id == id) = true
  · simp [hY] at hNe
  · have hYf : (y.id == id) = false := Bool.not_eq_true _ |>.mp hY
    simp [hYf]
    exact hy

theorem token_slot_ne_of_live_and_vacant
    {slots : List SlotState} {t : Token} {slotId : SlotId} {gen : Generation}
    (hLive : ∃ h : t.slot < slots.length, slots.get ⟨t.slot, h⟩ = .live t.generation)
    (hVacant : ∃ h : slotId < slots.length, slots.get ⟨slotId, h⟩ = .vacant gen) :
    t.slot ≠ slotId := by
  intro hEq
  subst hEq
  rcases hLive with ⟨h1, e1⟩
  rcases hVacant with ⟨h2, e2⟩
  simp only [List.get_eq_getElem] at e1 e2
  rw [e1] at e2
  contradiction

theorem token_ne_slot_of_distinct_live_tokens
    {reg : Registry.State} {t1 t2 : Token}
    (hNe : t1 ≠ t2)
    (hLive1 : TokenLive reg t1)
    (hLive2 : TokenLive reg t2) :
    t1.slot ≠ t2.slot := by
  intro hEqSlot
  rcases hLive1 with ⟨hSess1, ⟨hB1, e1⟩⟩
  rcases hLive2 with ⟨hSess2, ⟨hB2, e2⟩⟩
  simp only [List.get_eq_getElem] at e1 e2
  match t1, t2 with
  | ⟨s1, sl1, g1⟩, ⟨s2, sl2, g2⟩ =>
      dsimp at hSess1 hSess2 hEqSlot e1 e2
      subst hEqSlot
      rw [e1] at e2
      cases e2
      have hSessEq : s1 = s2 := by rw [hSess1, hSess2]
      subst hSessEq
      exact hNe rfl

theorem Step.initializerIdsUnique_preserved
    {s s' : State} {e : Event}
    (hInv : InitializerIdsUnique s)
    (hStep : Step s e s') :
    InitializerIdsUnique s' := by
  cases hStep with
  | beginPrepare => exact hInv
  | endPrepare => exact hInv
  | beginInitialize =>
      dsimp [InitializerIdsUnique] at hInv ⊢
      rw [List.pairwise_append]
      refine ⟨hInv, List.Pairwise.cons (fun x hx => False.elim (List.not_mem_nil hx)) List.Pairwise.nil, ?_⟩
      intro x hx y hy
      simp only [List.mem_singleton] at hy
      subst hy
      intro hEq
      subst hEq
      have hFind : s.initializers.find? (fun i => i.id == x.id) = none := by assumption
      have hMem : x ∈ s.initializers := hx
      have hFindSome : (s.initializers.find? (fun i => i.id == x.id)).isSome = true := by
        rw [List.find?_isSome]
        exact ⟨x, hMem, show (x.id == x.id) = true from beq_self_eq_true _⟩
      rw [hFind] at hFindSome
      contradiction
  | finishInitialize =>
      dsimp [InitializerIdsUnique, State.removeInitializer] at hInv ⊢
      exact List.Pairwise.filter (fun i => i.id != _) hInv
  | insertPendingFresh =>
      dsimp [InitializerIdsUnique, State.updateInitializer] at hInv ⊢
      exact List.Pairwise.map_update hInv (fun _ _ _ h => updateInitializer_id_ne h) hInv
  | insertPendingReuse =>
      dsimp [InitializerIdsUnique, State.updateInitializer] at hInv ⊢
      exact List.Pairwise.map_update hInv (fun _ _ _ h => updateInitializer_id_ne h) hInv
  | publishTopic =>
      dsimp [InitializerIdsUnique, State.updateInitializer] at hInv ⊢
      exact List.Pairwise.map_update hInv (fun _ _ _ h => updateInitializer_id_ne h) hInv
  | rollbackPendingReuse =>
      dsimp [InitializerIdsUnique, State.updateInitializer] at hInv ⊢
      exact List.Pairwise.map_update hInv (fun _ _ _ h => updateInitializer_id_ne h) hInv
  | rollbackPendingRetire =>
      dsimp [InitializerIdsUnique, State.updateInitializer] at hInv ⊢
      exact List.Pairwise.map_update hInv (fun _ _ _ h => updateInitializer_id_ne h) hInv
  | beginLookup => exact hInv
  | endLookup => exact hInv
  | sealTopics => exact hInv
  | closeRegistry => exact hInv
  | finishClose => exact hInv

theorem stageTokenNe_update_resolved
    {id : InitializerId} {a b : Initializer} (hNe : stageTokenNe a.stage b.stage) :
    stageTokenNe (if a.id == id then { a with stage := InitializerStage.resolved } else a).stage
                 (if b.id == id then { b with stage := InitializerStage.resolved } else b).stage := by
  by_cases hA : (a.id == id) = true <;> by_cases hB : (b.id == id) = true
  · simp [hA, hB, stageTokenNe]
  · have hB' : (b.id == id) = false := Bool.not_eq_true _ |>.mp hB
    simp [hA, hB', stageTokenNe]
  · have hA' : (a.id == id) = false := Bool.not_eq_true _ |>.mp hA
    simp [hA', hB, stageTokenNe]
  · have hA' : (a.id == id) = false := Bool.not_eq_true _ |>.mp hA
    have hB' : (b.id == id) = false := Bool.not_eq_true _ |>.mp hB
    simp [hA', hB']
    exact hNe

theorem Step.pendingTokensUnique_preserved
    {s s' : State} {e : Event}
    (hIds : InitializerIdsUnique s)
    (hRoots : PendingRootsValid s)
    (hInv : PendingTokensUnique s)
    (hStep : Step s e s') :
    PendingTokensUnique s' := by
  cases hStep with
  | beginPrepare => exact hInv
  | endPrepare => exact hInv
  | beginInitialize =>
      dsimp [PendingTokensUnique] at hInv ⊢
      rw [List.pairwise_append]
      refine ⟨hInv, List.Pairwise.cons (fun x hx => False.elim (List.not_mem_nil hx)) List.Pairwise.nil, ?_⟩
      intro x hx y hy
      simp only [List.mem_singleton] at hy
      subst hy
      dsimp [stageTokenNe]
      split <;> trivial
  | finishInitialize =>
      dsimp [PendingTokensUnique, State.removeInitializer] at hInv ⊢
      exact List.Pairwise.filter (fun i => i.id != _) hInv
  | insertPendingFresh =>
      rename_i freshId reg' hP hF hRegStep
      dsimp [PendingTokensUnique, State.updateInitializer] at hInv ⊢
      apply List.Pairwise.map_update hIds (newStage := .pending { session := s.registry.session, slot := s.registry.slots.length, generation := 1 })
      · intro a b hMemA hMemB hNeId hNe
        by_cases hA : (a.id == freshId) = true <;> by_cases hB : (b.id == freshId) = true
        · exfalso
          have h1 : a.id = freshId := beq_iff_eq.mp hA
          have h2 : b.id = freshId := beq_iff_eq.mp hB
          have h3 : a.id = b.id := by rw [h1, h2]
          exact hNeId h3
        · have hB' : (b.id == freshId) = false := Bool.not_eq_true _ |>.mp hB
          simp [hA, hB']
          cases hStageB : b.stage with
          | pending rt =>
              dsimp [stageTokenNe]
              intro hTokenEq
              subst hTokenEq
              have hRootB := hRoots b hMemB
              rw [hStageB] at hRootB
              rcases hRootB with ⟨_, ⟨hInBounds, _⟩⟩
              exact Nat.lt_irrefl _ hInBounds
          | _ => trivial
        · have hA' : (a.id == freshId) = false := Bool.not_eq_true _ |>.mp hA
          simp [hA', hB]
          cases hStageA : a.stage with
          | pending lt =>
              dsimp [stageTokenNe]
              intro hTokenEq
              subst hTokenEq
              have hRootA := hRoots a hMemA
              rw [hStageA] at hRootA
              rcases hRootA with ⟨_, ⟨hInBounds, _⟩⟩
              exact Nat.lt_irrefl _ hInBounds
          | _ => trivial
        · have hA' : (a.id == freshId) = false := Bool.not_eq_true _ |>.mp hA
          have hB' : (b.id == freshId) = false := Bool.not_eq_true _ |>.mp hB
          simp [hA', hB']
          exact hNe
      exact hInv
  | insertPendingReuse =>
      rename_i reuseId slotId gen reg' hP hF hRegStep
      dsimp [PendingTokensUnique, State.updateInitializer] at hInv ⊢
      apply List.Pairwise.map_update hIds (newStage := .pending { session := s.registry.session, slot := slotId, generation := gen })
      · intro a b hMemA hMemB hNeId hNe
        by_cases hA : (a.id == reuseId) = true <;> by_cases hB : (b.id == reuseId) = true
        · exfalso
          have h1 : a.id = reuseId := beq_iff_eq.mp hA
          have h2 : b.id = reuseId := beq_iff_eq.mp hB
          have h3 : a.id = b.id := by rw [h1, h2]
          exact hNeId h3
        · have hB' : (b.id == reuseId) = false := Bool.not_eq_true _ |>.mp hB
          simp [hA, hB']
          cases hStageB : b.stage with
          | pending rt =>
              dsimp [stageTokenNe]
              intro hTokenEq
              have hSlotEq : slotId = rt.slot := congrArg Token.slot hTokenEq
              subst hSlotEq
              have hRootB := hRoots b hMemB
              rw [hStageB] at hRootB
              rcases hRootB with ⟨_, ⟨hInBoundsB, hLiveB⟩⟩
              cases hRegStep
              rename_i hInBoundsVacant hVacant
              have hNeSlot := token_slot_ne_of_live_and_vacant ⟨hInBoundsB, hLiveB⟩ ⟨hInBoundsVacant, hVacant⟩
              exact hNeSlot rfl
          | _ => trivial
        · have hA' : (a.id == reuseId) = false := Bool.not_eq_true _ |>.mp hA
          simp [hA', hB]
          cases hStageA : a.stage with
          | pending lt =>
              dsimp [stageTokenNe]
              intro hTokenEq
              have hSlotEq : lt.slot = slotId := congrArg Token.slot hTokenEq
              subst hSlotEq
              have hRootA := hRoots a hMemA
              rw [hStageA] at hRootA
              rcases hRootA with ⟨_, ⟨hInBoundsA, hLiveA⟩⟩
              cases hRegStep
              rename_i hInBoundsVacant hVacant
              have hNeSlot := token_slot_ne_of_live_and_vacant ⟨hInBoundsA, hLiveA⟩ ⟨hInBoundsVacant, hVacant⟩
              exact hNeSlot rfl
          | _ => trivial
        · have hA' : (a.id == reuseId) = false := Bool.not_eq_true _ |>.mp hA
          have hB' : (b.id == reuseId) = false := Bool.not_eq_true _ |>.mp hB
          simp [hA', hB']
          exact hNe
      exact hInv
  | publishTopic =>
      dsimp [PendingTokensUnique, State.updateInitializer] at hInv ⊢
      exact List.Pairwise.map_update hIds (fun _ _ _ h => stageTokenNe_update_resolved h) hInv
  | rollbackPendingReuse =>
      dsimp [PendingTokensUnique, State.updateInitializer] at hInv ⊢
      exact List.Pairwise.map_update hIds (fun _ _ _ h => stageTokenNe_update_resolved h) hInv
  | rollbackPendingRetire =>
      dsimp [PendingTokensUnique, State.updateInitializer] at hInv ⊢
      exact List.Pairwise.map_update hIds (fun _ _ _ h => stageTokenNe_update_resolved h) hInv
  | beginLookup => exact hInv
  | endLookup => exact hInv
  | sealTopics => exact hInv
  | closeRegistry => exact hInv
  | finishClose => exact hInv

theorem Step.pendingRootsValid_preserved
    {s s' : State} {e : Event}
    (hToks : PendingTokensUnique s)
    (hInv : PendingRootsValid s)
    (hStep : Step s e s') :
    PendingRootsValid s' := by
  intro targetInit hMem
  cases hStep with
  | beginPrepare => exact hInv targetInit hMem
  | endPrepare => exact hInv targetInit hMem
  | beginInitialize =>
      simp only [List.mem_append, List.mem_singleton] at hMem
      cases hMem with
      | inl hIn => exact hInv targetInit hIn
      | inr hEq => subst hEq; dsimp
  | finishInitialize =>
      rename_i fId fInit hFind hStage
      dsimp [State.removeInitializer] at hMem
      have hIn : targetInit ∈ s.initializers := List.mem_of_mem_filter' hMem
      exact hInv targetInit hIn
  | publishTopic hPhase hFind =>
      rename_i pubId _
      dsimp [State.updateInitializer] at hMem
      rcases List.mem_map.mp hMem with ⟨orig, hOrigMem, rfl⟩
      by_cases hEq : (orig.id == pubId) = true
      · simp [hEq]
      · have hF' : (orig.id == pubId) = false := Bool.not_eq_true _ |>.mp hEq
        simp [hF']
        exact hInv orig hOrigMem
  | rollbackPendingReuse hFind hRegStep =>
      rename_i rbId rbToken _ _
      dsimp [State.updateInitializer] at hMem
      rcases List.mem_map.mp hMem with ⟨orig, hOrigMem, rfl⟩
      by_cases hEq : (orig.id == rbId) = true
      · simp [hEq]
      · have hF' : (orig.id == rbId) = false := Bool.not_eq_true _ |>.mp hEq
        simp [hF']
        cases hStage : orig.stage with
        | pending token =>
            have hOrigVal := hInv orig hOrigMem
            rw [hStage] at hOrigVal
            dsimp [TokenLive] at hOrigVal ⊢
            cases hRegStep
            rename_i _ hInBounds rbLive _
            rcases hOrigVal with ⟨hSess, ⟨hBounds, hLive⟩⟩
            refine ⟨hSess, ⟨?_, ?_⟩⟩
            · dsimp; rw [List.length_set]; exact hBounds
            · dsimp
              have hFindInit : { id := rbId, stage := InitializerStage.pending rbToken } ∈ s.initializers := List.mem_of_find?_eq_some' hFind
              have hOrigInit : orig ∈ s.initializers := hOrigMem
              have hNeId : orig.id ≠ rbId := by
                intro hIdEq
                have hEqB : (orig.id == rbId) = true := beq_iff_eq.mpr hIdEq
                rw [hEqB] at hF'
                contradiction
              have hTokenNe : token ≠ rbToken := by
                dsimp [PendingTokensUnique] at hToks
                cases List.Pairwise.mem_ne hToks hOrigInit hFindInit (fun hEq' => hNeId (by rw [hEq'])) with
                | inl h1 => rw [hStage] at h1; exact h1
                | inr h2 => rw [hStage] at h2; exact h2.symm
              have hNeSlot := token_ne_slot_of_distinct_live_tokens hTokenNe ⟨hSess, ⟨hBounds, hLive⟩⟩ (by
                have hV := hInv _ hFindInit
                dsimp at hV
                exact hV)
              rw [List.getElem_set_ne hNeSlot.symm]
              exact hLive
        | _ => trivial
  | rollbackPendingRetire hFind hRegStep =>
      rename_i rbId rbToken _
      dsimp [State.updateInitializer] at hMem
      rcases List.mem_map.mp hMem with ⟨orig, hOrigMem, rfl⟩
      by_cases hEq : (orig.id == rbId) = true
      · simp [hEq]
      · have hF' : (orig.id == rbId) = false := Bool.not_eq_true _ |>.mp hEq
        simp [hF']
        cases hStage : orig.stage with
        | pending token =>
            have hOrigVal := hInv orig hOrigMem
            rw [hStage] at hOrigVal
            dsimp [TokenLive] at hOrigVal ⊢
            cases hRegStep
            rename_i _ hInBounds rbLive _
            rcases hOrigVal with ⟨hSess, ⟨hBounds, hLive⟩⟩
            refine ⟨hSess, ⟨?_, ?_⟩⟩
            · dsimp; rw [List.length_set]
              exact hBounds
            · dsimp
              have hFindInit : { id := rbId, stage := InitializerStage.pending rbToken } ∈ s.initializers := List.mem_of_find?_eq_some' hFind
              have hOrigInit : orig ∈ s.initializers := hOrigMem
              have hNeId : orig.id ≠ rbId := by
                intro hIdEq
                have hEqB : (orig.id == rbId) = true := beq_iff_eq.mpr hIdEq
                rw [hEqB] at hF'
                contradiction
              have hTokenNe : token ≠ rbToken := by
                dsimp [PendingTokensUnique] at hToks
                cases List.Pairwise.mem_ne hToks hOrigInit hFindInit (fun hEq' => hNeId (by rw [hEq'])) with
                | inl h1 => rw [hStage] at h1; exact h1
                | inr h2 => rw [hStage] at h2; exact h2.symm
              have hNeSlot := token_ne_slot_of_distinct_live_tokens hTokenNe ⟨hSess, ⟨hBounds, hLive⟩⟩ (by
                have hV := hInv _ hFindInit
                dsimp at hV
                exact hV)
              rw [List.getElem_set_ne hNeSlot.symm]
              exact hLive
        | _ => trivial
  | insertPendingFresh =>
      rename_i freshId reg' hP hF hRegStep
      dsimp [State.updateInitializer] at hMem
      rcases List.mem_map.mp hMem with ⟨orig, hOrigMem, rfl⟩
      by_cases hEq : (orig.id == freshId) = true
      · simp [hEq]
        dsimp [TokenLive]
        cases hRegStep
        refine ⟨by rfl, ⟨?_, ?_⟩⟩
        · rw [List.length_append]; exact Nat.lt_succ_self _
        · dsimp
          simp [List.getElem_append_right, Nat.sub_self]
      · have hF' : (orig.id == freshId) = false := Bool.not_eq_true _ |>.mp hEq
        simp [hF']
        cases hStage : orig.stage with
        | pending token =>
            have hOrigVal := hInv orig hOrigMem
            rw [hStage] at hOrigVal
            dsimp [TokenLive] at hOrigVal ⊢
            rcases hOrigVal with ⟨hSess, ⟨hBounds, hLive⟩⟩
            cases hRegStep
            refine ⟨hSess, ⟨?_, ?_⟩⟩
            · rw [List.length_append]
              exact Nat.lt_add_right 1 hBounds
            · rw [List.getElem_append_left hBounds]
              exact hLive
        | _ => trivial
  | insertPendingReuse =>
      rename_i reuseId slotId gen reg' hP hF hRegStep
      dsimp [State.updateInitializer] at hMem
      rcases List.mem_map.mp hMem with ⟨orig, hOrigMem, rfl⟩
      by_cases hEq : (orig.id == reuseId) = true
      · simp [hEq]
        dsimp [TokenLive]
        cases hRegStep
        rename_i hInBounds hVacant
        refine ⟨by rfl, ⟨?_, ?_⟩⟩
        · rw [List.length_set]; exact hInBounds
        · exact List.getElem_set_self _
      · have hF' : (orig.id == reuseId) = false := Bool.not_eq_true _ |>.mp hEq
        simp [hF']
        cases hStage : orig.stage with
        | pending token =>
            have hOrigVal := hInv orig hOrigMem
            rw [hStage] at hOrigVal
            dsimp [TokenLive] at hOrigVal ⊢
            rcases hOrigVal with ⟨hSess, ⟨hBounds, hLive⟩⟩
            cases hRegStep
            rename_i hInBounds hVacant
            refine ⟨hSess, ⟨?_, ?_⟩⟩
            · rw [List.length_set]
              exact hBounds
            · have hNeSlot : token.slot ≠ slotId := by
                intro hEqSlot
                subst hEqSlot
                simp only [List.get_eq_getElem] at hVacant hLive
                rw [hLive] at hVacant
                contradiction
              rw [List.getElem_set_ne hNeSlot.symm]
              exact hLive
        | _ => trivial
  | beginLookup hRegStep =>
      cases hRegStep
      exact hInv targetInit hMem
  | endLookup hRegStep =>
      cases hRegStep
      exact hInv targetInit hMem
  | sealTopics => exact hInv targetInit hMem
  | closeRegistry hPhase hNoInits hNoPrepares hRegStep =>
      rw [hNoInits] at hMem
      contradiction
  | finishClose hPhase hRegStep =>
      cases hRegStep
      exact hInv targetInit hMem

theorem Step.runtimeInvariant_preserved
    {s s' : State} {e : Event}
    (hInv : RuntimeInvariant s)
    (hStep : Step s e s') :
    RuntimeInvariant s' := by
  rcases hInv with ⟨hPhase, hOp, hIds, hToks, hRoots⟩
  refine ⟨Step.phaseInvariant_preserved hPhase hStep,
          Step.operationInvariant_preserved hOp hStep,
          Step.initializerIdsUnique_preserved hIds hStep,
          Step.pendingTokensUnique_preserved hIds hRoots hToks hStep,
          Step.pendingRootsValid_preserved hToks hRoots hStep⟩

theorem Reachable.runtimeInvariant_preserved
    {s t : State}
    (hInv : RuntimeInvariant s)
    (hReach : Reachable s t) :
    RuntimeInvariant t := by
  induction hReach with
  | refl => exact hInv
  | tail _ hStep ih => exact Step.runtimeInvariant_preserved ih hStep

end XlFnFormal.Handle.Runtime
