import XlFnFormal.Handle.Topics.Transition

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Topics

theorem mem_of_mem_filter_topics {α : Type} {p : α → Bool} {x : α} {l : List α}
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

theorem pairwise_filter_topics {α : Type} {R : α → α → Prop} (p : α → Bool)
    {l : List α} (h : l.Pairwise R) : (l.filter p).Pairwise R := by
  induction h with
  | nil => exact List.Pairwise.nil
  | cons hHead hTail ih =>
      dsimp [List.filter]
      split
      · refine List.Pairwise.cons ?_ ih
        intro x hx
        exact hHead x (mem_of_mem_filter_topics hx)
      · exact ih

theorem pairwise_append_singleton_topics
    {α : Type} {R : α → α → Prop} {l : List α} {x : α}
    (hPair : l.Pairwise R)
    (hSep : ∀ y ∈ l, R y x) :
    (l ++ [x]).Pairwise R := by
  rw [List.pairwise_append]
  refine ⟨hPair,
    List.Pairwise.cons (fun y hy => False.elim (List.not_mem_nil hy)) List.Pairwise.nil,
    ?_⟩
  intro y hy z hz
  simp only [List.mem_singleton] at hz
  subst z
  exact hSep y hy

theorem pairwise_map_topics
    {α : Type} {R : α → α → Prop} {l : List α} {f : α → α}
    (hPair : l.Pairwise R)
    (hRel : ∀ a ∈ l, ∀ b ∈ l, R a b → R (f a) (f b)) :
    (l.map f).Pairwise R := by
  induction hPair with
  | nil => exact List.Pairwise.nil
  | cons hHead hTail ih =>
      simp only [List.map]
      refine List.Pairwise.cons ?_ (ih (fun a hA b hB hR =>
        hRel a (List.mem_cons_of_mem _ hA) b (List.mem_cons_of_mem _ hB) hR))
      intro x hx
      rcases List.mem_map.mp hx with ⟨y, hy, rfl⟩
      exact hRel _ List.mem_cons_self _ (List.mem_cons_of_mem _ hy) (hHead y hy)

theorem pairwise_mem_ne_topics
    {α : Type} {R : α → α → Prop} {x y : α} {l : List α}
    (hPair : l.Pairwise R) (hX : x ∈ l) (hY : y ∈ l) (hNe : x ≠ y) :
    R x y ∨ R y x := by
  induction hPair with
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
          | inl hY1 => subst hY1; right; exact hHead x hX2
          | inr hY2 => exact ih hX2 hY2

theorem initial_invariant (session : Registry.SessionId) :
    (initialState session).Invariant := by
  refine ⟨Runtime.initial_runtimeInvariant session,
    List.Pairwise.nil, List.Pairwise.nil, ?_, List.Pairwise.nil,
    List.Pairwise.nil, List.Pairwise.nil, ?_, ?_, ?_, ?_⟩
  · intro init hMem
    contradiction
  · intro entry hMem
    contradiction
  · intro topic hMem
    contradiction
  · intro topic hMem
    contradiction
  · intro topic hMem
    contradiction

theorem no_topic_member
    {s : State} {key : TopicKey} {topic : Topic}
    (hNoTopic : s.findTopic? key = none)
    (hMem : topic ∈ s.byKey) :
    topic.key ≠ key := by
  intro hEq
  dsimp [State.findTopic?] at hNoTopic
  have hSome : (s.byKey.find? (fun candidate => candidate.key == key)).isSome = true := by
    rw [List.find?_isSome]
    exact ⟨topic, hMem, beq_iff_eq.mpr hEq⟩
  rw [hNoTopic] at hSome
  contradiction

theorem no_initializer_member
    {s : State} {key : TopicKey} {init : Initializer}
    (hNoInitializer : s.findInitializing? key = none)
    (hMem : init ∈ s.initializing) :
    init.key ≠ key := by
  intro hEq
  dsimp [State.findInitializing?] at hNoInitializer
  have hSome : (s.initializing.find? (fun candidate => candidate.key == key)).isSome = true := by
    rw [List.find?_isSome]
    exact ⟨init, hMem, beq_iff_eq.mpr hEq⟩
  rw [hNoInitializer] at hSome
  contradiction

theorem mem_of_findTopic_some
    {s : State} {key : TopicKey} {topic : Topic}
    (hFind : s.findTopic? key = some topic) :
    topic ∈ s.byKey := by
  dsimp [State.findTopic?] at hFind
  exact Runtime.List.mem_of_find?_eq_some' hFind

theorem mem_of_findReverse_some
    {s : State} {rtdKey : RtdKey} {entry : ReverseTopic}
    (hFind : s.findReverse? rtdKey = some entry) :
    entry ∈ s.byRtdKey := by
  dsimp [State.findReverse?] at hFind
  exact Runtime.List.mem_of_find?_eq_some' hFind

theorem mem_of_findInitializing_some
    {s : State} {key : TopicKey} {init : Initializer}
    (hFind : s.findInitializing? key = some init) :
    init ∈ s.initializing := by
  dsimp [State.findInitializing?] at hFind
  exact Runtime.List.mem_of_find?_eq_some' hFind

theorem runtime_mem_updateInitializer_same_id
    {inits : List Runtime.Initializer} {target : Runtime.InitializerId}
    {stage : Runtime.InitializerStage} {runtimeInit : Runtime.Initializer}
    (hMem : runtimeInit ∈ inits) :
    ∃ updated ∈ inits.map (fun i => if i.id == target then { i with stage := stage } else i),
      updated.id = runtimeInit.id := by
  let updated : Runtime.Initializer :=
    if runtimeInit.id == target then { runtimeInit with stage := stage } else runtimeInit
  refine ⟨updated, ?_, ?_⟩
  · exact List.mem_map.mpr ⟨runtimeInit, hMem, rfl⟩
  · dsimp [updated]
    by_cases h : runtimeInit.id == target <;> simp [h]

theorem runtime_mem_filter_ne
    {inits : List Runtime.Initializer} {target : Runtime.InitializerId}
    {runtimeInit : Runtime.Initializer}
    (hMem : runtimeInit ∈ inits)
    (hNe : runtimeInit.id ≠ target) :
    runtimeInit ∈ inits.filter (fun i => i.id != target) := by
  apply List.mem_filter.mpr
  refine ⟨hMem, ?_⟩
  simp [hNe]

theorem runtime_update_id_pred
    {i : Runtime.Initializer} {id target : Runtime.InitializerId}
    {stage : Runtime.InitializerStage} :
    ((if i.id == target then { i with stage := stage } else i).id == id) =
      (i.id == id) := by
  by_cases h : i.id == target <;> simp [h]

theorem runtime_find_update_ne
    {inits : List Runtime.Initializer} {id target : Runtime.InitializerId}
    {stage : Runtime.InitializerStage} {found : Runtime.Initializer}
    (hFind : inits.find? (fun i => i.id == id) = some found)
    (hNe : id ≠ target) :
    (inits.map (fun i => if i.id == target then { i with stage := stage } else i)).find?
        (fun i => i.id == id) = some found := by
  induction inits with
  | nil => simp at hFind
  | cons head tail ih =>
      simp only [List.map, List.find?] at hFind ⊢
      rw [runtime_update_id_pred]
      by_cases hId : (head.id == id) = true
      · by_cases hTarget : (head.id == target) = true
        · have hIdEq : head.id = id := beq_iff_eq.mp hId
          have hTargetEq : head.id = target := beq_iff_eq.mp hTarget
          exact False.elim (hNe (hIdEq.symm.trans hTargetEq))
        · have hTargetFalse : (head.id == target) = false :=
            Bool.not_eq_true _ |>.mp hTarget
          simp only [hId, hTargetFalse] at hFind ⊢
          exact hFind
      · have hIdFalse : (head.id == id) = false := Bool.not_eq_true _ |>.mp hId
        simp only [hIdFalse] at hFind ⊢
        exact ih hFind

theorem runtime_find_remove_ne
    {inits : List Runtime.Initializer} {id target : Runtime.InitializerId}
    {found : Runtime.Initializer}
    (hFind : inits.find? (fun i => i.id == id) = some found)
    (hNe : id ≠ target) :
    (inits.filter (fun i => i.id != target)).find? (fun i => i.id == id) = some found := by
  induction inits with
  | nil => simp at hFind
  | cons head tail ih =>
      by_cases hId : (head.id == id) = true
      · have hIdEq : head.id = id := beq_iff_eq.mp hId
        have hTarget : (head.id != target) = true := by
          simp [hNe, hIdEq]
        simpa [List.filter, List.find?, hId, hTarget] using hFind
      · have hIdFalse : (head.id == id) = false := Bool.not_eq_true _ |>.mp hId
        have hFindTail : tail.find? (fun i => i.id == id) = some found := by
          simpa [List.find?, hIdFalse] using hFind
        by_cases hKeep : (head.id != target) = true
        · simpa [List.filter, List.find?, hIdFalse, hKeep] using ih hFindTail
        · simpa [List.filter, List.find?, hIdFalse, hKeep] using ih hFindTail

theorem updateTopicStage_pairwise_keys
    {s : State} {key : TopicKey} {stage : TopicStage}
    (hInv : s.VisibleKeysUnique) :
    ({ s with byKey := s.updateTopicStage key stage }).VisibleKeysUnique := by
  dsimp [State.VisibleKeysUnique, State.updateTopicStage] at hInv ⊢
  apply pairwise_map_topics hInv
  intro a hA b hB hRel
  by_cases hAKey : (a.key == key) = true <;>
    by_cases hBKey : (b.key == key) = true <;>
    simp [hAKey, hBKey]
  all_goals exact hRel

theorem updateTopicStage_pairwise_tokens
    {s : State} {key : TopicKey} {stage : TopicStage}
    (hInv : s.VisibleTokensUnique) :
    ({ s with byKey := s.updateTopicStage key stage }).VisibleTokensUnique := by
  dsimp [State.VisibleTokensUnique, State.updateTopicStage] at hInv ⊢
  apply pairwise_map_topics hInv
  intro a hA b hB hRel
  by_cases hAKey : (a.key == key) = true <;>
    by_cases hBKey : (b.key == key) = true <;>
    simp [hAKey, hBKey]
  all_goals exact hRel

theorem Step.initializingKeysUnique_preserved
    {s s' : State} {e : Event}
    (hInv : s.InitializingKeysUnique)
    (hStep : Step s e s') :
    s'.InitializingKeysUnique := by
  cases hStep with
  | beginInitializer hNoTopic hNoInitializer hNoRuntimeId hRuntime =>
      dsimp [State.InitializingKeysUnique] at hInv ⊢
      apply pairwise_append_singleton_topics hInv
      intro init hMem
      exact no_initializer_member hNoInitializer hMem
  | finishInitializer =>
      dsimp [State.InitializingKeysUnique, State.removeInitializing] at hInv ⊢
      exact pairwise_filter_topics (fun init => init.runtimeId != _) hInv
  | insertPendingFresh | insertPendingReuse | publishVisible | commitPublication |
      withdrawVisible | rollbackPendingReuse | rollbackPendingRetire |
      beginPrepare | endPrepare | sealTopics | beginLookup | endLookup | closeRegistry |
      finishClose => exact hInv

theorem Step.initializerIdsUnique_preserved
    {s s' : State} {e : Event}
    (hInv : s.InitializerIdsUnique)
    (hStep : Step s e s') :
    s'.InitializerIdsUnique := by
  cases hStep with
  | beginInitializer hNoTopic hNoInitializer hNoRuntimeId hRuntime =>
      dsimp [State.InitializerIdsUnique] at hInv ⊢
      apply pairwise_append_singleton_topics hInv
      intro init hMem
      exact hNoRuntimeId init hMem
  | finishInitializer =>
      dsimp [State.InitializerIdsUnique, State.removeInitializing] at hInv ⊢
      exact pairwise_filter_topics (fun init => init.runtimeId != _) hInv
  | insertPendingFresh | insertPendingReuse | publishVisible | commitPublication |
      withdrawVisible | rollbackPendingReuse | rollbackPendingRetire |
      beginPrepare | endPrepare | sealTopics | beginLookup | endLookup | closeRegistry |
      finishClose => exact hInv

theorem Step.visibleKeysUnique_preserved
    {s s' : State} {e : Event}
    (hInv : s.VisibleKeysUnique)
    (hStep : Step s e s') :
    s'.VisibleKeysUnique := by
  cases hStep with
  | beginInitializer => exact hInv
  | publishVisible hPhase hInit hNoTopic hNoRtdKey hNoToken hPending hRoot =>
      dsimp [State.VisibleKeysUnique] at hInv ⊢
      apply pairwise_append_singleton_topics hInv
      intro topic hMem
      exact no_topic_member hNoTopic hMem
  | commitPublication hInit hTopic hTopicKey hPending hRuntime =>
      exact updateTopicStage_pairwise_keys hInv
  | withdrawVisible hInit hTopic hTopicKey hPending =>
      dsimp [State.VisibleKeysUnique, State.removeTopic] at hInv ⊢
      exact pairwise_filter_topics (fun topic => topic.key != _) hInv
  | sealTopics => exact List.Pairwise.nil
  | insertPendingFresh | insertPendingReuse | rollbackPendingReuse |
      rollbackPendingRetire | finishInitializer | beginPrepare | endPrepare |
      beginLookup | endLookup | closeRegistry | finishClose => exact hInv

theorem Step.visibleTokensUnique_preserved
    {s s' : State} {e : Event}
    (hInv : s.VisibleTokensUnique)
    (hStep : Step s e s') :
    s'.VisibleTokensUnique := by
  cases hStep with
  | beginInitializer => exact hInv
  | publishVisible hPhase hInit hNoTopic hNoRtdKey hNoToken hPending hRoot =>
      dsimp [State.VisibleTokensUnique] at hInv ⊢
      apply pairwise_append_singleton_topics hInv
      intro topic hMem
      exact hNoToken topic hMem
  | commitPublication hInit hTopic hTopicKey hPending hRuntime =>
      exact updateTopicStage_pairwise_tokens hInv
  | withdrawVisible hInit hTopic hTopicKey hPending =>
      dsimp [State.VisibleTokensUnique, State.removeTopic] at hInv ⊢
      exact pairwise_filter_topics (fun topic => topic.key != _) hInv
  | sealTopics => exact List.Pairwise.nil
  | insertPendingFresh | insertPendingReuse | rollbackPendingReuse |
      rollbackPendingRetire | finishInitializer | beginPrepare | endPrepare |
      beginLookup | endLookup | closeRegistry | finishClose => exact hInv

theorem runtimeTokenLive_preserved_insertFresh
    {s s' : Runtime.State} {id : Runtime.InitializerId} {token : Registry.Token}
    (hLive : Runtime.TokenLive s.registry token)
    (hStep : Runtime.Step s (.insertPendingFresh id) s') :
    Runtime.TokenLive s'.registry token := by
  cases hStep with
  | insertPendingFresh hPhase hFind hRegStep =>
      cases hRegStep with
      | insertFresh hMay =>
          rcases hLive with ⟨hSession, ⟨hBounds, hSlot⟩⟩
          refine ⟨hSession, ⟨?_, ?_⟩⟩
          · rw [List.length_append]
            exact Nat.lt_add_right 1 hBounds
          · dsimp
            rw [List.getElem_append_left hBounds]
            exact hSlot

theorem runtimeTokenLive_preserved_insertReuse
    {s s' : Runtime.State} {id : Runtime.InitializerId}
    {slot : Registry.SlotId} {generation : Registry.Generation}
    {token : Registry.Token}
    (hLive : Runtime.TokenLive s.registry token)
    (hStep : Runtime.Step s (.insertPendingReuse id slot generation) s') :
    Runtime.TokenLive s'.registry token := by
  cases hStep with
  | insertPendingReuse hPhase hFind hRegStep =>
      cases hRegStep with
      | insertReuse hMay hInBounds hVacant =>
          rcases hLive with ⟨hSession, ⟨hBounds, hSlot⟩⟩
          have hSlotNe : token.slot ≠ slot := by
            intro hEq
            subst hEq
            simp only [List.get_eq_getElem] at hVacant hSlot
            rw [hSlot] at hVacant
            contradiction
          refine ⟨hSession, ⟨?_, ?_⟩⟩
          · rw [List.length_set]
            exact hBounds
          · dsimp
            rw [List.getElem_set_ne hSlotNe.symm]
            exact hSlot

theorem runtimeTokenLive_preserved_lookup
    {s s' : Runtime.State} {token : Registry.Token}
    (hLive : Runtime.TokenLive s.registry token)
    (hStep : Runtime.Step s (.beginLookup token) s') :
    Runtime.TokenLive s'.registry token := by
  cases hStep with
  | beginLookup hRegStep =>
      cases hRegStep
      exact hLive

theorem runtimeTokenLive_preserved_endLookup
    {s s' : Runtime.State}
    {token : Registry.Token}
    (hLive : Runtime.TokenLive s.registry token)
    (hStep : Runtime.Step s .endLookup s') :
    Runtime.TokenLive s'.registry token := by
  cases hStep with
  | endLookup hRegStep =>
      cases hRegStep
      exact hLive

theorem Step.runtimeInvariant_preserved
    {s s' : State} {e : Event}
    (hInv : Runtime.RuntimeInvariant s.runtime)
    (hStep : Step s e s') :
    Runtime.RuntimeInvariant s'.runtime := by
  cases hStep with
  | beginPrepare hRuntime =>
      exact Runtime.Step.runtimeInvariant_preserved hInv hRuntime
  | endPrepare hRuntime =>
      exact Runtime.Step.runtimeInvariant_preserved hInv hRuntime
  | sealTopics hRuntime =>
      exact Runtime.Step.runtimeInvariant_preserved hInv hRuntime
  | beginLookup hRuntime =>
      exact Runtime.Step.runtimeInvariant_preserved hInv hRuntime
  | endLookup hRuntime =>
      exact Runtime.Step.runtimeInvariant_preserved hInv hRuntime
  | beginInitializer hNoTopic hNoInitializer hNoRuntimeId hRuntime =>
      exact Runtime.Step.runtimeInvariant_preserved hInv hRuntime
  | insertPendingFresh hInit hNoTopic hRuntime =>
      exact Runtime.Step.runtimeInvariant_preserved hInv hRuntime
  | insertPendingReuse hInit hNoTopic hRuntime =>
      exact Runtime.Step.runtimeInvariant_preserved hInv hRuntime
  | publishVisible => exact hInv
  | commitPublication hInit hTopic hTopicKey hPending hRuntime =>
      exact Runtime.Step.runtimeInvariant_preserved hInv hRuntime
  | withdrawVisible => exact hInv
  | rollbackPendingReuse hInit hNoTopic hNoToken hPending hRuntime =>
      exact Runtime.Step.runtimeInvariant_preserved hInv hRuntime
  | rollbackPendingRetire hInit hNoTopic hNoToken hPending hRuntime =>
      exact Runtime.Step.runtimeInvariant_preserved hInv hRuntime
  | finishInitializer hInit hReady hRuntime =>
      exact Runtime.Step.runtimeInvariant_preserved hInv hRuntime
  | closeRegistry hNoVisible hNoReverse hNoInitializers hRuntime =>
      exact Runtime.Step.runtimeInvariant_preserved hInv hRuntime
  | finishClose hRuntime =>
      exact Runtime.Step.runtimeInvariant_preserved hInv hRuntime

theorem Step.initializersBackedByRuntime_preserved
    {s s' : State} {e : Event}
    (hInv : s.InitializersBackedByRuntime)
    (hStep : Step s e s') :
    s'.InitializersBackedByRuntime := by
  cases hStep with
  | beginInitializer hNoTopic hNoInitializer hNoRuntimeId hRuntime =>
      rename_i key runtimeId
      cases hRuntime
      intro init hMem
      simp only [List.mem_append, List.mem_singleton] at hMem
      cases hMem with
      | inl hOld =>
          rcases hInv init hOld with ⟨runtimeInit, hRuntimeMem, hId⟩
          exact ⟨runtimeInit, List.mem_append_left _ hRuntimeMem, hId⟩
      | inr hNew =>
          subst hNew
          exact ⟨{ id := runtimeId, stage := .beforeInsert },
            List.mem_append_right _ (List.mem_singleton_self _), rfl⟩
  | insertPendingFresh hInit hNoTopic hRuntime =>
      cases hRuntime with
      | insertPendingFresh hPhase hFind hRegStep =>
          intro init hMem
          rcases hInv init hMem with ⟨runtimeInit, hRuntimeMem, hId⟩
          rcases runtime_mem_updateInitializer_same_id hRuntimeMem with
            ⟨updated, hUpdatedMem, hUpdatedId⟩
          exact ⟨updated, hUpdatedMem, hUpdatedId.trans hId⟩
  | insertPendingReuse hInit hNoTopic hRuntime =>
      cases hRuntime with
      | insertPendingReuse hPhase hFind hRegStep =>
          intro init hMem
          rcases hInv init hMem with ⟨runtimeInit, hRuntimeMem, hId⟩
          rcases runtime_mem_updateInitializer_same_id hRuntimeMem with
            ⟨updated, hUpdatedMem, hUpdatedId⟩
          exact ⟨updated, hUpdatedMem, hUpdatedId.trans hId⟩
  | commitPublication hInit hTopic hTopicKey hPending hRuntime =>
      cases hRuntime with
      | publishTopic hPhase hFind =>
          intro init hMem
          rcases hInv init hMem with ⟨runtimeInit, hRuntimeMem, hId⟩
          rcases runtime_mem_updateInitializer_same_id hRuntimeMem with
            ⟨updated, hUpdatedMem, hUpdatedId⟩
          exact ⟨updated, hUpdatedMem, hUpdatedId.trans hId⟩
  | rollbackPendingReuse hInit hNoTopic hNoToken hPending hRuntime =>
      cases hRuntime with
      | rollbackPendingReuse hFind hRegStep =>
          intro init hMem
          rcases hInv init hMem with ⟨runtimeInit, hRuntimeMem, hId⟩
          rcases runtime_mem_updateInitializer_same_id hRuntimeMem with
            ⟨updated, hUpdatedMem, hUpdatedId⟩
          exact ⟨updated, hUpdatedMem, hUpdatedId.trans hId⟩
  | rollbackPendingRetire hInit hNoTopic hNoToken hPending hRuntime =>
      cases hRuntime with
      | rollbackPendingRetire hFind hRegStep =>
          intro init hMem
          rcases hInv init hMem with ⟨runtimeInit, hRuntimeMem, hId⟩
          rcases runtime_mem_updateInitializer_same_id hRuntimeMem with
            ⟨updated, hUpdatedMem, hUpdatedId⟩
          exact ⟨updated, hUpdatedMem, hUpdatedId.trans hId⟩
  | finishInitializer hInit hReady hRuntime =>
      rename_i key runtimeId
      cases hRuntime with
      | finishInitialize hFind hStage =>
          intro init hMem
          rcases List.mem_filter.mp hMem with ⟨hOld, hFilter⟩
          have hInitNe : init.runtimeId ≠ runtimeId := by
            intro hEq
            have hFalse : (init.runtimeId != runtimeId) = false := by simp [hEq]
            rw [hFalse] at hFilter
            contradiction
          rcases hInv init hOld with ⟨runtimeInit, hRuntimeMem, hId⟩
          have hRuntimeNe : runtimeInit.id ≠ runtimeId := by
            intro hEq
            apply hInitNe
            exact hId.symm.trans hEq
          exact ⟨runtimeInit, runtime_mem_filter_ne hRuntimeMem hRuntimeNe, hId⟩
  | closeRegistry hNoVisible hNoReverse hNoInitializers hRuntime =>
      intro init hMem
      rw [hNoInitializers] at hMem
      contradiction
  | beginPrepare hRuntime => cases hRuntime; exact hInv
  | endPrepare hRuntime => cases hRuntime; exact hInv
  | publishVisible => exact hInv
  | withdrawVisible => exact hInv
  | beginLookup hRuntime => cases hRuntime; exact hInv
  | endLookup hRuntime => cases hRuntime; exact hInv
  | sealTopics hRuntime => cases hRuntime; exact hInv
  | finishClose hRuntime => cases hRuntime; exact hInv

theorem visibleRootsValid_after_insertFresh
    {s : State} {runtime' : Runtime.State} {id : Runtime.InitializerId}
    (hRoots : s.VisibleTopicRootsValid)
    (hStep : Runtime.Step s.runtime (.insertPendingFresh id) runtime') :
    ∀ topic ∈ s.byKey, Runtime.TokenLive runtime'.registry topic.token := by
  intro topic hMem
  exact runtimeTokenLive_preserved_insertFresh (hRoots topic hMem) hStep

theorem visibleRootsValid_after_insertReuse
    {s : State} {runtime' : Runtime.State} {id : Runtime.InitializerId}
    {slot : Registry.SlotId} {generation : Registry.Generation}
    (hRoots : s.VisibleTopicRootsValid)
    (hStep : Runtime.Step s.runtime
      (.insertPendingReuse id slot generation) runtime') :
    ∀ topic ∈ s.byKey, Runtime.TokenLive runtime'.registry topic.token := by
  intro topic hMem
  exact runtimeTokenLive_preserved_insertReuse (hRoots topic hMem) hStep

theorem visibleRootsValid_after_rollbackReuse
    {s : State} {runtime' : Runtime.State} {id : Runtime.InitializerId}
    {nextGeneration : Registry.Generation} {token : Registry.Token}
    (hRoots : s.VisibleTopicRootsValid)
    (hNoToken : ∀ topic ∈ s.byKey, topic.token ≠ token)
    (hPending : s.runtime.findInitializer? id =
      some { id := id, stage := .pending token })
    (hStep : Runtime.Step s.runtime
      (.rollbackPendingReuse id nextGeneration) runtime') :
    ∀ topic ∈ s.byKey, Runtime.TokenLive runtime'.registry topic.token := by
  intro old hOldMem
  have hOldLive := hRoots old hOldMem
  cases hStep with
  | rollbackPendingReuse hFind hRegStep =>
      rw [hPending] at hFind
      cases hFind
      cases hRegStep with
      | removeReuse hAuth hInBounds hRemovedLive hNext =>
          change token.session = s.runtime.registry.session at hAuth
          have hTokenLive : Runtime.TokenLive s.runtime.registry token :=
            ⟨hAuth, ⟨hInBounds, hRemovedLive⟩⟩
          have hSlotNe : old.token.slot ≠ token.slot :=
            Runtime.token_ne_slot_of_distinct_live_tokens
              (hNoToken old hOldMem) hOldLive hTokenLive
          rcases hOldLive with ⟨hSession, ⟨hBounds, hSlot⟩⟩
          refine ⟨hSession, ⟨?_, ?_⟩⟩
          · rw [List.length_set]
            exact hBounds
          · dsimp
            rw [List.getElem_set_ne hSlotNe.symm]
            exact hSlot

theorem visibleRootsValid_after_rollbackRetire
    {s : State} {runtime' : Runtime.State} {id : Runtime.InitializerId}
    {token : Registry.Token}
    (hRoots : s.VisibleTopicRootsValid)
    (hNoToken : ∀ topic ∈ s.byKey, topic.token ≠ token)
    (hPending : s.runtime.findInitializer? id =
      some { id := id, stage := .pending token })
    (hStep : Runtime.Step s.runtime (.rollbackPendingRetire id) runtime') :
    ∀ topic ∈ s.byKey, Runtime.TokenLive runtime'.registry topic.token := by
  intro old hOldMem
  have hOldLive := hRoots old hOldMem
  cases hStep with
  | rollbackPendingRetire hFind hRegStep =>
      rw [hPending] at hFind
      cases hFind
      cases hRegStep with
      | removeRetire hAuth hInBounds hRemovedLive hExhausted =>
          change token.session = s.runtime.registry.session at hAuth
          have hTokenLive : Runtime.TokenLive s.runtime.registry token :=
            ⟨hAuth, ⟨hInBounds, hRemovedLive⟩⟩
          have hSlotNe : old.token.slot ≠ token.slot :=
            Runtime.token_ne_slot_of_distinct_live_tokens
              (hNoToken old hOldMem) hOldLive hTokenLive
          rcases hOldLive with ⟨hSession, ⟨hBounds, hSlot⟩⟩
          refine ⟨hSession, ⟨?_, ?_⟩⟩
          · rw [List.length_set]
            exact hBounds
          · dsimp
            rw [List.getElem_set_ne hSlotNe.symm]
            exact hSlot

theorem Step.visibleTopicRootsValid_preserved
    {s s' : State} {e : Event}
    (hInv : s.VisibleTopicRootsValid)
    (hStep : Step s e s') :
    s'.VisibleTopicRootsValid := by
  cases hStep with
  | beginPrepare hRuntime =>
      cases hRuntime
      exact hInv
  | endPrepare hRuntime =>
      cases hRuntime
      exact hInv
  | sealTopics hRuntime =>
      intro topic hMem
      contradiction
  | beginLookup hRuntime =>
      cases hRuntime with
      | beginLookup hRegStep =>
          cases hRegStep
          exact hInv
  | endLookup hRuntime =>
      cases hRuntime with
      | endLookup hRegStep =>
          cases hRegStep
          exact hInv
  | beginInitializer hNoTopic hNoInitializer hNoRuntimeId hRuntime =>
      cases hRuntime
      exact hInv
  | insertPendingFresh hInit hNoTopic hRuntime =>
      exact visibleRootsValid_after_insertFresh (s := s) hInv hRuntime
  | insertPendingReuse hInit hNoTopic hRuntime =>
      exact visibleRootsValid_after_insertReuse (s := s) hInv hRuntime
  | publishVisible hPhase hInit hNoTopic hNoRtdKey hNoToken hPending hRoot =>
      intro topic hMem
      simp only [List.mem_append, List.mem_singleton] at hMem
      cases hMem with
      | inl hOld => exact hInv topic hOld
      | inr hNew =>
          subst hNew
          exact hRoot
  | commitPublication hInit hTopic hTopicKey hPending hRuntime =>
      rename_i topic0 key runtimeId
      intro topic hMem
      rcases List.mem_map.mp hMem with ⟨old, hOldMem, rfl⟩
      cases hRuntime
      by_cases hKey : old.key = key
      · simpa [hKey] using hInv old hOldMem
      · simpa [hKey] using hInv old hOldMem
  | withdrawVisible hInit hTopic hTopicKey hPending =>
      intro topic hMem
      exact hInv topic (mem_of_mem_filter_topics hMem)
  | rollbackPendingReuse hInit hNoTopic hNoToken hPending hRuntime =>
      exact visibleRootsValid_after_rollbackReuse (s := s) hInv hNoToken hPending hRuntime
  | rollbackPendingRetire hInit hNoTopic hNoToken hPending hRuntime =>
      exact visibleRootsValid_after_rollbackRetire (s := s) hInv hNoToken hPending hRuntime
  | finishInitializer hInit hReady hRuntime =>
      cases hRuntime
      exact hInv
  | closeRegistry hNoVisible hNoReverse hNoInitializers hRuntime =>
      intro topic hMem
      rw [hNoVisible] at hMem
      contradiction
  | finishClose hRuntime =>
      cases hRuntime
      exact hInv

theorem initializer_ids_ne_of_distinct_keys
    {s : State} {lhs rhs : Initializer}
    (hIds : s.InitializerIdsUnique)
    (hLhs : lhs ∈ s.initializing)
    (hRhs : rhs ∈ s.initializing)
    (hKeyNe : lhs.key ≠ rhs.key) :
    lhs.runtimeId ≠ rhs.runtimeId := by
  intro hIdEq
  have hLhsNeRhs : lhs ≠ rhs := by
    intro hEq
    apply hKeyNe
    cases hEq
    rfl
  have hRelation := pairwise_mem_ne_topics hIds hLhs hRhs hLhsNeRhs
  cases hRelation with
  | inl h => exact h hIdEq
  | inr h => exact h hIdEq.symm

theorem provisionalTopics_after_runtime_update
    {s : State} {runtime' : Runtime.State}
    {key : TopicKey} {runtimeId : Runtime.InitializerId}
    {stage : Runtime.InitializerStage}
    (hKeys : s.InitializingKeysUnique)
    (hIds : s.InitializerIdsUnique)
    (hProv : s.ProvisionalTopicsHavePendingRoots)
    (hInitFind : s.findInitializing? key =
      some { runtimeId := runtimeId, key := key })
    (hTopicKeyNe : ∀ topic ∈ s.byKey, topic.stage = .provisional →
      topic.key ≠ key)
    (hUpdate : runtime'.initializers =
      s.runtime.initializers.map
        (fun i => if i.id == runtimeId then { i with stage := stage } else i)) :
    ∀ topic ∈ s.byKey, topic.stage = .provisional →
      ∃ init ∈ s.initializing,
        init.key = topic.key ∧
        runtime'.findInitializer? init.runtimeId =
          some { id := init.runtimeId, stage := .pending topic.token } := by
  intro topic hTopicMem hStage
  rcases hProv topic hTopicMem hStage with
    ⟨init, hInitMem, hInitKey, hPending⟩
  have hTargetMem : ({ runtimeId := runtimeId, key := key } : Initializer) ∈
      s.initializing := mem_of_findInitializing_some hInitFind
  have hKeyNe : init.key ≠ key := by
    rw [hInitKey]
    exact hTopicKeyNe topic hTopicMem hStage
  have hIdNe := initializer_ids_ne_of_distinct_keys hIds hInitMem hTargetMem hKeyNe
  refine ⟨init, hInitMem, hInitKey, ?_⟩
  dsimp [Runtime.State.findInitializer?]
  rw [hUpdate]
  exact runtime_find_update_ne hPending hIdNe

theorem provisionalTopics_after_runtime_remove
    {s : State} {runtime' : Runtime.State}
    {key : TopicKey} {runtimeId : Runtime.InitializerId}
    (hIds : s.InitializerIdsUnique)
    (hProv : s.ProvisionalTopicsHavePendingRoots)
    (hTargetMem : ({ runtimeId := runtimeId, key := key } : Initializer) ∈
      s.initializing)
    (hTopicKeyNe : ∀ topic ∈ s.byKey, topic.stage = .provisional →
      topic.key ≠ key)
    (hUpdate : runtime'.initializers =
      s.runtime.initializers.filter (fun i => i.id != runtimeId)) :
    ∀ topic ∈ s.byKey, topic.stage = .provisional →
      ∃ init ∈ s.initializing.filter (fun i => i.runtimeId != runtimeId),
        init.key = topic.key ∧
        runtime'.findInitializer? init.runtimeId =
          some { id := init.runtimeId, stage := .pending topic.token } := by
  intro topic hTopicMem hStage
  rcases hProv topic hTopicMem hStage with
    ⟨init, hInitMem, hInitKey, hPending⟩
  have hKeyNe : init.key ≠ key := by
    rw [hInitKey]
    exact hTopicKeyNe topic hTopicMem hStage
  have hInitNe : init.runtimeId ≠ runtimeId := by
    intro hEq
    have hLhsNeRhs := initializer_ids_ne_of_distinct_keys hIds hInitMem hTargetMem hKeyNe
    exact hLhsNeRhs hEq
  have hInitFiltered : init ∈ s.initializing.filter (fun i => i.runtimeId != runtimeId) :=
    List.mem_filter.mpr ⟨hInitMem, by simp [hInitNe]⟩
  refine ⟨init, hInitFiltered, hInitKey, ?_⟩
  dsimp [Runtime.State.findInitializer?]
  rw [hUpdate]
  exact runtime_find_remove_ne hPending hInitNe

theorem Step.provisionalTopicsHavePendingRoots_preserved
    {s s' : State} {e : Event}
    (hKeys : s.InitializingKeysUnique)
    (hIds : s.InitializerIdsUnique)
    (hProv : s.ProvisionalTopicsHavePendingRoots)
    (hStep : Step s e s') :
    s'.ProvisionalTopicsHavePendingRoots := by
  cases hStep with
  | beginPrepare hRuntime =>
      cases hRuntime
      exact hProv
  | endPrepare hRuntime =>
      cases hRuntime
      exact hProv
  | sealTopics hRuntime =>
      intro topic hMem
      contradiction
  | beginLookup hRuntime =>
      cases hRuntime
      exact hProv
  | endLookup hRuntime =>
      cases hRuntime
      exact hProv
  | beginInitializer hNoTopic hNoInitializer hNoRuntimeId hRuntime =>
      rename_i key runtimeId
      cases hRuntime
      intro topic hTopicMem hStage
      rcases hProv topic hTopicMem hStage with
        ⟨init, hInitMem, hInitKey, hPending⟩
      refine ⟨init, List.mem_append_left _ hInitMem, hInitKey, ?_⟩
      dsimp [Runtime.State.findInitializer?]
      dsimp [Runtime.State.findInitializer?] at hPending
      rw [List.find?_append, hPending]
      rfl
  | insertPendingFresh hInit hNoTopic hRuntime =>
      rename_i key runtimeId
      cases hRuntime with
      | insertPendingFresh hPhase hFind hRegStep =>
          apply provisionalTopics_after_runtime_update hKeys hIds hProv hInit
          · intro topic hMem hStage
            exact no_topic_member hNoTopic hMem
          · rfl
  | insertPendingReuse hInit hNoTopic hRuntime =>
      rename_i key runtimeId slot generation
      cases hRuntime with
      | insertPendingReuse hPhase hFind hRegStep =>
          apply provisionalTopics_after_runtime_update hKeys hIds hProv hInit
          · intro topic hMem hStage
            exact no_topic_member hNoTopic hMem
          · rfl
  | publishVisible hPhase hInit hNoTopic hNoRtdKey hNoToken hPending hRoot =>
      rename_i key runtimeId rtdKey
      intro topic hTopicMem hStage
      simp only [List.mem_append, List.mem_singleton] at hTopicMem
      cases hTopicMem with
      | inl hOld => exact hProv topic hOld hStage
      | inr hNew =>
          subst hNew
          refine ⟨{ runtimeId := runtimeId, key := key },
            mem_of_findInitializing_some hInit, rfl, ?_⟩
          exact hPending
  | commitPublication hInit hTopic hTopicKey hPending hRuntime =>
      rename_i topic0 key runtimeId
      cases hRuntime with
      | publishTopic hPhase hFind =>
          intro topic hTopicMem hStage
          rcases List.mem_map.mp hTopicMem with ⟨old, hOldMem, rfl⟩
          by_cases hKey : (old.key == key) = true
          · simp [hKey] at hStage
          · have hOldStage : old.stage = .provisional := by
              simpa [hKey] using hStage
            rcases hProv old hOldMem hOldStage with
              ⟨init, hInitMem, hInitKey, hOldPending⟩
            have hTargetMem : ({ runtimeId := runtimeId, key := key } : Initializer) ∈
                s.initializing := mem_of_findInitializing_some hInit
            have hKeyNe : init.key ≠ key := by
              rw [hInitKey]
              intro hEq
              apply hKey
              exact beq_iff_eq.mpr hEq
            have hIdNe := initializer_ids_ne_of_distinct_keys
              hIds hInitMem hTargetMem hKeyNe
            refine ⟨init, hInitMem, ?_, ?_⟩
            · simpa [hKey] using hInitKey
            dsimp [Runtime.State.findInitializer?]
            dsimp [Runtime.State.findInitializer?] at hOldPending
            simp [hKey]
            exact runtime_find_update_ne hOldPending hIdNe
  | withdrawVisible hInit hTopic hTopicKey hPending =>
      intro topic hTopicMem hStage
      exact hProv topic (mem_of_mem_filter_topics hTopicMem) hStage
  | rollbackPendingReuse hInit hNoTopic hNoToken hPending hRuntime =>
      rename_i key runtimeId nextGeneration
      cases hRuntime with
      | rollbackPendingReuse hFind hRegStep =>
          apply provisionalTopics_after_runtime_update hKeys hIds hProv hInit
          · intro topic hMem hStage
            exact no_topic_member hNoTopic hMem
          · rfl
  | rollbackPendingRetire hInit hNoTopic hNoToken hPending hRuntime =>
      rename_i key runtimeId
      cases hRuntime with
      | rollbackPendingRetire hFind hRegStep =>
          apply provisionalTopics_after_runtime_update hKeys hIds hProv hInit
          · intro topic hMem hStage
            exact no_topic_member hNoTopic hMem
          · rfl
  | finishInitializer hInit hReady hRuntime =>
      rename_i key runtimeId
      cases hRuntime with
      | finishInitialize hFind hStageRuntime =>
          apply provisionalTopics_after_runtime_remove hIds hProv
          · exact mem_of_findInitializing_some hInit
          · intro topic hMem hStageTopic hKeyEq
            have hCommitted := hReady topic hMem hKeyEq
            exact (by cases hStageTopic.symm.trans hCommitted)
          · rfl
  | closeRegistry hNoVisible hNoReverse hNoInitializers hRuntime =>
      intro topic hMem hStage
      rw [hNoVisible] at hMem
      contradiction
  | finishClose hRuntime =>
      cases hRuntime
      exact hProv

theorem no_reverse_member
    {s : State} {rtdKey : RtdKey} {entry : ReverseTopic}
    (hNoReverse : s.findReverse? rtdKey = none)
    (hMem : entry ∈ s.byRtdKey) :
    entry.rtdKey ≠ rtdKey := by
  intro hEq
  dsimp [State.findReverse?] at hNoReverse
  have hSome : (s.byRtdKey.find? (fun candidate => candidate.rtdKey == rtdKey)).isSome = true := by
    rw [List.find?_isSome]
    exact ⟨entry, hMem, beq_iff_eq.mpr hEq⟩
  rw [hNoReverse] at hSome
  contradiction

theorem mem_of_mem_filter_reverse
    {rtdKey : RtdKey} {entry : ReverseTopic} {entries : List ReverseTopic}
    (h : entry ∈ entries.filter (fun candidate => candidate.rtdKey != rtdKey)) :
    entry ∈ entries := by
  induction entries with
  | nil => contradiction
  | cons head tail ih =>
      dsimp [List.filter] at h
      split at h
      · cases List.mem_cons.mp h with
        | inl hHead => subst hHead; exact List.mem_cons_self
        | inr hTail => exact List.mem_cons_of_mem head (ih hTail)
      · exact List.mem_cons_of_mem head (ih h)

theorem updateTopicStage_pairwise_rtdKeys
    {s : State} {key : TopicKey} {stage : TopicStage}
    (hInv : s.RtdKeysUnique) :
    ({ s with byKey := s.updateTopicStage key stage }).RtdKeysUnique := by
  dsimp [State.RtdKeysUnique, State.updateTopicStage] at hInv ⊢
  apply pairwise_map_topics hInv
  intro a hA b hB hRel
  by_cases hAKey : (a.key == key) = true <;>
    by_cases hBKey : (b.key == key) = true <;>
    simp [hAKey, hBKey]
  all_goals exact hRel

theorem Step.rtdKeysUnique_preserved
    {s s' : State} {e : Event}
    (hInv : s.RtdKeysUnique)
    (hComplete : s.ReverseMapComplete)
    (hStep : Step s e s') :
    s'.RtdKeysUnique := by
  cases hStep with
  | beginInitializer => exact hInv
  | publishVisible hPhase hInit hNoTopic hNoRtdKey hNoToken hPending hRoot =>
      rename_i key runtimeId rtdKey
      dsimp [State.RtdKeysUnique] at hInv ⊢
      apply pairwise_append_singleton_topics hInv
      intro topic hMem
      intro hEq
      rcases hComplete topic hMem with ⟨entry, hEntryMem, hEntryKey, hEntryRtd⟩
      exact no_reverse_member hNoRtdKey hEntryMem (hEntryRtd.trans hEq)
  | commitPublication hInit hTopic hTopicKey hPending hRuntime =>
      exact updateTopicStage_pairwise_rtdKeys hInv
  | withdrawVisible hInit hTopic hTopicKey hPending =>
      dsimp [State.RtdKeysUnique, State.removeTopic] at hInv ⊢
      exact pairwise_filter_topics (fun topic => topic.key != _) hInv
  | sealTopics => exact List.Pairwise.nil
  | insertPendingFresh | insertPendingReuse | rollbackPendingReuse |
      rollbackPendingRetire | finishInitializer | beginPrepare | endPrepare |
      beginLookup | endLookup | closeRegistry | finishClose => exact hInv

theorem distinct_topics_have_rtd_keys
    {s : State} {left right : Topic}
    (hInv : s.RtdKeysUnique)
    (hLeft : left ∈ s.byKey)
    (hRight : right ∈ s.byKey)
    (hNe : left ≠ right) :
    left.rtdKey ≠ right.rtdKey := by
  have hRelation := pairwise_mem_ne_topics hInv hLeft hRight hNe
  cases hRelation with
  | inl hNotEqual => exact hNotEqual
  | inr hNotEqual => exact hNotEqual.symm

theorem Step.reverseMapSound_preserved
    {s s' : State} {e : Event}
    (hSound : s.ReverseMapSound)
    (hVisibleKeys : s.VisibleKeysUnique)
    (hStep : Step s e s') :
    s'.ReverseMapSound := by
  cases hStep with
  | beginPrepare hRuntime => cases hRuntime; exact hSound
  | endPrepare hRuntime => cases hRuntime; exact hSound
  | sealTopics hRuntime =>
      intro entry hMem
      contradiction
  | beginLookup hRuntime => cases hRuntime; exact hSound
  | endLookup hRuntime => cases hRuntime; exact hSound
  | beginInitializer hNoTopic hNoInitializer hNoRuntimeId hRuntime =>
      cases hRuntime
      exact hSound
  | insertPendingFresh hInit hNoTopic hRuntime =>
      cases hRuntime
      exact hSound
  | insertPendingReuse hInit hNoTopic hRuntime =>
      cases hRuntime
      exact hSound
  | publishVisible hPhase hInit hNoTopic hNoRtdKey hNoToken hPending hRoot =>
      rename_i token key runtimeId rtdKey
      intro entry hMem
      simp only [List.mem_append, List.mem_singleton] at hMem
      cases hMem with
      | inl hOld =>
          rcases hSound entry hOld with ⟨topic, hTopicMem, hTopicKey, hTopicRtd⟩
          exact ⟨topic, List.mem_append_left _ hTopicMem, hTopicKey, hTopicRtd⟩
      | inr hNew =>
          subst hNew
          refine ⟨{ key := key, rtdKey := rtdKey, token := token, stage := .provisional },
            ?_, rfl, rfl⟩
          simp
  | commitPublication hInit hTopic hTopicKey hPending hRuntime =>
      rename_i source key runtimeId
      intro entry hMem
      rcases hSound entry hMem with ⟨old, hOldMem, hOldKey, hOldRtd⟩
      by_cases h : old.key == key
      · refine ⟨{ old with stage := .committed }, ?_, ?_, ?_⟩
        · apply List.mem_map.mpr
          exact ⟨old, hOldMem, by simp [h]⟩
        · exact hOldKey
        · exact hOldRtd
      · refine ⟨old, ?_, hOldKey, hOldRtd⟩
        apply List.mem_map.mpr
        exact ⟨old, hOldMem, by simp [h]⟩
  | withdrawVisible hInit hTopic hTopicKey hPending =>
      rename_i target key runtimeId
      let target' : Topic := { target with stage := .provisional }
      have hTargetMem : target' ∈ s.byKey := mem_of_findTopic_some hTopic
      intro entry hMem
      rcases List.mem_filter.mp hMem with ⟨hEntryMem, hEntryNe⟩
      rcases hSound entry hEntryMem with ⟨old, hOldMem, hOldKey, hOldRtd⟩
      have hEntryRtdNe : entry.rtdKey ≠ target'.rtdKey := by
        intro hEq
        have hFalse : (entry.rtdKey != target.rtdKey) = false := by
          simp [target', hEq]
        rw [hFalse] at hEntryNe
        contradiction
      have hOldKeyNe : old.key ≠ key := by
        intro hEq
        have hOldTarget : old = target' := by
          by_cases hSame : old = target'
          · exact hSame
          · exfalso
            have hRelation := pairwise_mem_ne_topics hVisibleKeys hOldMem
              hTargetMem hSame
            have hKeyRelation : old.key = target'.key := by
              simpa [target'] using hEq.trans hTopicKey.symm
            cases hRelation with
            | inl hNotEqual => exact hNotEqual hKeyRelation
            | inr hNotEqual => exact hNotEqual hKeyRelation.symm
        subst hOldTarget
        exact hEntryRtdNe (hOldRtd.symm.trans rfl)
      refine ⟨old, ?_, hOldKey, hOldRtd⟩
      apply List.mem_filter.mpr
      exact ⟨hOldMem, by simp [hOldKeyNe]⟩
  | rollbackPendingReuse hInit hNoTopic hNoToken hPending hRuntime =>
      cases hRuntime
      exact hSound
  | rollbackPendingRetire hInit hNoTopic hNoToken hPending hRuntime =>
      cases hRuntime
      exact hSound
  | finishInitializer hInit hReady hRuntime =>
      cases hRuntime
      exact hSound
  | closeRegistry hNoVisible hNoReverse hNoInitializers hRuntime =>
      intro entry hMem
      rw [hNoReverse] at hMem
      contradiction
  | finishClose hRuntime => cases hRuntime; exact hSound

theorem Step.reverseMapComplete_preserved
    {s s' : State} {e : Event}
    (hComplete : s.ReverseMapComplete)
    (hRtdKeys : s.RtdKeysUnique)
    (hVisibleKeys : s.VisibleKeysUnique)
    (hStep : Step s e s') :
    s'.ReverseMapComplete := by
  cases hStep with
  | beginPrepare hRuntime => cases hRuntime; exact hComplete
  | endPrepare hRuntime => cases hRuntime; exact hComplete
  | sealTopics hRuntime =>
      intro topic hMem
      contradiction
  | beginLookup hRuntime => cases hRuntime; exact hComplete
  | endLookup hRuntime => cases hRuntime; exact hComplete
  | beginInitializer hNoTopic hNoInitializer hNoRuntimeId hRuntime =>
      cases hRuntime
      exact hComplete
  | insertPendingFresh hInit hNoTopic hRuntime =>
      cases hRuntime
      exact hComplete
  | insertPendingReuse hInit hNoTopic hRuntime =>
      cases hRuntime
      exact hComplete
  | publishVisible hPhase hInit hNoTopic hNoRtdKey hNoToken hPending hRoot =>
      rename_i key runtimeId rtdKey
      intro topic hMem
      simp only [List.mem_append, List.mem_singleton] at hMem
      cases hMem with
      | inl hOld =>
          rcases hComplete topic hOld with ⟨entry, hEntryMem, hEntryKey, hEntryRtd⟩
          exact ⟨entry, List.mem_append_left _ hEntryMem, hEntryKey, hEntryRtd⟩
      | inr hNew =>
          subst hNew
          exact ⟨{ rtdKey := rtdKey, key := key },
            List.mem_append_right _ (List.mem_singleton_self _), rfl, rfl⟩
  | commitPublication hInit hTopic hTopicKey hPending hRuntime =>
      rename_i source key runtimeId
      intro topic hMem
      rcases List.mem_map.mp hMem with ⟨old, hOldMem, rfl⟩
      rcases hComplete old hOldMem with ⟨entry, hEntryMem, hEntryKey, hEntryRtd⟩
      refine ⟨entry, hEntryMem, ?_, ?_⟩
      · by_cases h : old.key == key <;> simp [h, hEntryKey]
      · by_cases h : old.key == key <;> simp [h, hEntryRtd]
  | withdrawVisible hInit hTopic hTopicKey hPending =>
      rename_i target key runtimeId
      let target' : Topic := { target with stage := .provisional }
      have hTargetMem : target' ∈ s.byKey := mem_of_findTopic_some hTopic
      intro topic hMem
      rcases List.mem_filter.mp hMem with ⟨hOldMem, hOldKeyNeBool⟩
      have hOldKeyNe : topic.key ≠ key := by
        intro hEq
        have hFalse : (topic.key != key) = false := by simp [hEq]
        rw [hFalse] at hOldKeyNeBool
        contradiction
      have hOldNeTarget' : topic ≠ target' := by
        intro hEq
        apply hOldKeyNe
        have hKeyEq : topic.key = target'.key := congrArg Topic.key hEq
        simpa [target'] using hKeyEq.trans hTopicKey
      have hRtdNe' : topic.rtdKey ≠ target'.rtdKey :=
        distinct_topics_have_rtd_keys hRtdKeys hOldMem
          hTargetMem hOldNeTarget'
      have hRtdNe : topic.rtdKey ≠ target.rtdKey := by
        simpa [target'] using hRtdNe'
      rcases hComplete topic hOldMem with ⟨entry, hEntryMem, hEntryKey, hEntryRtd⟩
      refine ⟨entry, ?_, hEntryKey, hEntryRtd⟩
      apply List.mem_filter.mpr
      exact ⟨hEntryMem, by simp [hRtdNe, hEntryRtd]⟩
  | rollbackPendingReuse hInit hNoTopic hNoToken hPending hRuntime =>
      cases hRuntime
      exact hComplete
  | rollbackPendingRetire hInit hNoTopic hNoToken hPending hRuntime =>
      cases hRuntime
      exact hComplete
  | finishInitializer hInit hReady hRuntime =>
      cases hRuntime
      exact hComplete
  | closeRegistry hNoVisible hNoReverse hNoInitializers hRuntime =>
      intro topic hMem
      rw [hNoVisible] at hMem
      contradiction
  | finishClose hRuntime => cases hRuntime; exact hComplete

theorem Step.invariant_preserved
    {s s' : State} {e : Event}
    (hInv : s.Invariant)
    (hStep : Step s e s') :
    s'.Invariant := by
  rcases hInv with
    ⟨hRuntime, hKeys, hIds, hBacked, hVisibleKeys, hVisibleTokens, hRtdKeys,
      hReverseSound, hReverseComplete, hRoots, hProv⟩
  exact ⟨
    Step.runtimeInvariant_preserved hRuntime hStep,
    Step.initializingKeysUnique_preserved hKeys hStep,
    Step.initializerIdsUnique_preserved hIds hStep,
    Step.initializersBackedByRuntime_preserved hBacked hStep,
    Step.visibleKeysUnique_preserved hVisibleKeys hStep,
    Step.visibleTokensUnique_preserved hVisibleTokens hStep,
    Step.rtdKeysUnique_preserved hRtdKeys hReverseComplete hStep,
    Step.reverseMapSound_preserved hReverseSound hVisibleKeys hStep,
    Step.reverseMapComplete_preserved hReverseComplete hRtdKeys hVisibleKeys hStep,
    Step.visibleTopicRootsValid_preserved hRoots hStep,
    Step.provisionalTopicsHavePendingRoots_preserved hKeys hIds hProv hStep⟩

theorem Reachable.invariant_preserved
    {s t : State}
    (hInv : s.Invariant)
    (hReach : Reachable s t) :
    t.Invariant := by
  induction hReach with
  | refl => exact hInv
  | tail _ hStep ih => exact Step.invariant_preserved ih hStep

theorem reachable_invariant (session : Registry.SessionId) {s : State}
    (hReach : Reachable (initialState session) s) :
    s.Invariant := by
  exact Reachable.invariant_preserved (initial_invariant session) hReach

end XlFnFormal.Handle.Topics
