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
    List.Pairwise.nil, List.Pairwise.nil, List.Pairwise.nil, List.Pairwise.nil, ?_, ?_⟩
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

theorem mem_of_findInitializer_some
    {s : State} {key : TopicKey} {init : Initializer}
    (hFind : s.findInitializing? key = some init) :
    init ∈ s.initializing := by
  dsimp [State.findInitializing?] at hFind
  exact Runtime.List.mem_of_find?_eq_some' hFind

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
  | publishVisibleFresh => exact hInv
  | publishVisibleReuse => exact hInv
  | commitPublication => exact hInv
  | rollbackVisibleReuse =>
      dsimp [State.InitializingKeysUnique, State.removeInitializing] at hInv ⊢
      exact hInv
  | rollbackVisibleRetire =>
      dsimp [State.InitializingKeysUnique, State.removeInitializing] at hInv ⊢
      exact hInv
  | finishInitializer hInit hReady hRuntime =>
      dsimp [State.InitializingKeysUnique, State.removeInitializing] at hInv ⊢
      exact pairwise_filter_topics (fun init => init.runtimeId != _) hInv
  | beginPrepare => exact hInv
  | endPrepare => exact hInv
  | sealTopics => exact hInv
  | beginLookup => exact hInv
  | endLookup => exact hInv

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
  | finishInitializer hInit hReady hRuntime =>
      dsimp [State.InitializerIdsUnique, State.removeInitializing] at hInv ⊢
      exact pairwise_filter_topics (fun init => init.runtimeId != _) hInv
  | rollbackVisibleReuse => exact hInv
  | rollbackVisibleRetire => exact hInv
  | publishVisibleFresh => exact hInv
  | publishVisibleReuse => exact hInv
  | commitPublication => exact hInv
  | beginPrepare => exact hInv
  | endPrepare => exact hInv
  | sealTopics => exact hInv
  | beginLookup => exact hInv
  | endLookup => exact hInv

theorem Step.visibleKeysUnique_preserved
    {s s' : State} {e : Event}
    (hInv : s.VisibleKeysUnique)
    (hStep : Step s e s') :
    s'.VisibleKeysUnique := by
  cases hStep with
  | publishVisibleFresh hInit hNoTopic hNoToken hRuntime hPending hRoot =>
      dsimp [State.VisibleKeysUnique] at hInv ⊢
      apply pairwise_append_singleton_topics hInv
      intro topic hMem
      exact no_topic_member hNoTopic hMem
  | publishVisibleReuse hInit hNoTopic hNoToken hRuntime hPending hRoot =>
      dsimp [State.VisibleKeysUnique] at hInv ⊢
      apply pairwise_append_singleton_topics hInv
      intro topic hMem
      exact no_topic_member hNoTopic hMem
  | commitPublication hInit hTopic hRuntime =>
      exact updateTopicStage_pairwise_keys hInv
  | rollbackVisibleReuse hInit hTopic hRuntime =>
      dsimp [State.VisibleKeysUnique, State.removeTopic] at hInv ⊢
      exact pairwise_filter_topics (fun topic => topic.key != _) hInv
  | rollbackVisibleRetire hInit hTopic hRuntime =>
      dsimp [State.VisibleKeysUnique, State.removeTopic] at hInv ⊢
      exact pairwise_filter_topics (fun topic => topic.key != _) hInv
  | finishInitializer => exact hInv
  | beginPrepare => exact hInv
  | endPrepare => exact hInv
  | sealTopics => exact hInv
  | beginLookup => exact hInv
  | endLookup => exact hInv
  | beginInitializer => exact hInv

theorem Step.visibleTokensUnique_preserved
    {s s' : State} {e : Event}
    (hInv : s.VisibleTokensUnique)
    (hStep : Step s e s') :
    s'.VisibleTokensUnique := by
  cases hStep with
  | publishVisibleFresh hInit hNoTopic hNoToken hRuntime hPending hRoot =>
      dsimp [State.VisibleTokensUnique] at hInv ⊢
      apply pairwise_append_singleton_topics hInv
      intro topic hMem
      intro hSame
      exact hNoToken topic hMem hSame
  | publishVisibleReuse hInit hNoTopic hNoToken hRuntime hPending hRoot =>
      dsimp [State.VisibleTokensUnique] at hInv ⊢
      apply pairwise_append_singleton_topics hInv
      intro topic hMem
      intro hSame
      exact hNoToken topic hMem hSame
  | commitPublication hInit hTopic hRuntime =>
      exact updateTopicStage_pairwise_tokens hInv
  | rollbackVisibleReuse hInit hTopic hRuntime =>
      dsimp [State.VisibleTokensUnique, State.removeTopic] at hInv ⊢
      exact pairwise_filter_topics (fun topic => topic.key != _) hInv
  | rollbackVisibleRetire hInit hTopic hRuntime =>
      dsimp [State.VisibleTokensUnique, State.removeTopic] at hInv ⊢
      exact pairwise_filter_topics (fun topic => topic.key != _) hInv
  | finishInitializer => exact hInv
  | beginPrepare => exact hInv
  | endPrepare => exact hInv
  | sealTopics => exact hInv
  | beginLookup => exact hInv
  | endLookup => exact hInv
  | beginInitializer => exact hInv

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

theorem runtimeTokenLive_preserved_publish
    {s s' : Runtime.State} {id : Runtime.InitializerId} {token : Registry.Token}
    (hLive : Runtime.TokenLive s.registry token)
    (hStep : Runtime.Step s (.publishTopic id) s') :
    Runtime.TokenLive s'.registry token := by
  cases hStep
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
  | publishVisibleFresh hInit hNoTopic hNoToken hRuntime hPending hRoot =>
      exact Runtime.Step.runtimeInvariant_preserved hInv hRuntime
  | publishVisibleReuse hInit hNoTopic hNoToken hRuntime hPending hRoot =>
      exact Runtime.Step.runtimeInvariant_preserved hInv hRuntime
  | commitPublication hInit hTopic hTopicKey hPending hRuntime =>
      exact Runtime.Step.runtimeInvariant_preserved hInv hRuntime
  | rollbackVisibleReuse hInit hTopic hTopicKey hPending hRuntime =>
      exact Runtime.Step.runtimeInvariant_preserved hInv hRuntime
  | rollbackVisibleRetire hInit hTopic hTopicKey hPending hRuntime =>
      exact Runtime.Step.runtimeInvariant_preserved hInv hRuntime
  | finishInitializer hInit hReady hRuntime =>
      exact Runtime.Step.runtimeInvariant_preserved hInv hRuntime

theorem mem_of_findTopic_some
    {s : State} {key : TopicKey} {topic : Topic}
    (hFind : s.findTopic? key = some topic) :
    topic ∈ s.byKey := by
  dsimp [State.findTopic?] at hFind
  exact Runtime.List.mem_of_find?_eq_some' hFind

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
    {key : TopicKey} {nextGeneration : Registry.Generation} {target : Topic}
    (hRoots : s.VisibleTopicRootsValid)
    (hTokens : s.VisibleTokensUnique)
    (hTargetMem : target ∈ s.byKey)
    (hTargetKey : target.key = key)
    (hPending : s.runtime.findInitializer? id =
      some { id := id, stage := .pending target.token })
    (hStep : Runtime.Step s.runtime
      (.rollbackPendingReuse id nextGeneration) runtime') :
    ∀ topic ∈ s.removeTopic key, Runtime.TokenLive runtime'.registry topic.token := by
  intro old hOldMem
  have hOldMem' := mem_of_mem_filter_topics hOldMem
  have hOldKeyNe : old.key ≠ key := by
    dsimp [State.removeTopic] at hOldMem
    have hPred : (old.key != key) = true := by
      rcases List.mem_filter.mp hOldMem with ⟨_, h⟩
      exact h
    intro hEq
    have hFalse : (old.key != key) = false := by simp [hEq]
    rw [hFalse] at hPred
    contradiction
  have hOldNeTarget : old ≠ target := by
    intro hEq
    subst hEq
    exact hOldKeyNe hTargetKey
  have hTokenNe : old.token ≠ target.token := by
    have hRelation := pairwise_mem_ne_topics hTokens hOldMem' hTargetMem hOldNeTarget
    cases hRelation with
    | inl h => exact h
    | inr h => exact h.symm
  cases hStep with
  | rollbackPendingReuse hFind hRegStep =>
      cases hRegStep with
      | removeReuse hAuth hInBounds hRemovedLive hNext =>
          rw [hPending] at hFind
          cases hFind
          have hOldLive := hRoots old hOldMem'
          have hSlotNe : old.token.slot ≠ target.token.slot := by
            exact Runtime.token_ne_slot_of_distinct_live_tokens hTokenNe hOldLive
              ⟨hAuth, ⟨hInBounds, hRemovedLive⟩⟩
          rcases hOldLive with ⟨hSession, ⟨hBounds, hSlot⟩⟩
          refine ⟨hSession, ⟨?_, ?_⟩⟩
          · rw [List.length_set]
            exact hBounds
          · dsimp
            rw [List.getElem_set_ne hSlotNe.symm]
            exact hSlot

theorem visibleRootsValid_after_rollbackRetire
    {s : State} {runtime' : Runtime.State} {id : Runtime.InitializerId}
    {key : TopicKey} {target : Topic}
    (hRoots : s.VisibleTopicRootsValid)
    (hTokens : s.VisibleTokensUnique)
    (hTargetMem : target ∈ s.byKey)
    (hTargetKey : target.key = key)
    (hPending : s.runtime.findInitializer? id =
      some { id := id, stage := .pending target.token })
    (hStep : Runtime.Step s.runtime (.rollbackPendingRetire id) runtime') :
    ∀ topic ∈ s.removeTopic key, Runtime.TokenLive runtime'.registry topic.token := by
  intro old hOldMem
  have hOldMem' := mem_of_mem_filter_topics hOldMem
  have hOldKeyNe : old.key ≠ key := by
    dsimp [State.removeTopic] at hOldMem
    have hPred : (old.key != key) = true := by
      rcases List.mem_filter.mp hOldMem with ⟨_, h⟩
      exact h
    intro hEq
    have hFalse : (old.key != key) = false := by simp [hEq]
    rw [hFalse] at hPred
    contradiction
  have hOldNeTarget : old ≠ target := by
    intro hEq
    subst hEq
    exact hOldKeyNe hTargetKey
  have hTokenNe : old.token ≠ target.token := by
    have hRelation := pairwise_mem_ne_topics hTokens hOldMem' hTargetMem hOldNeTarget
    cases hRelation with
    | inl h => exact h
    | inr h => exact h.symm
  cases hStep with
  | rollbackPendingRetire hFind hRegStep =>
      cases hRegStep with
      | removeRetire hAuth hInBounds hRemovedLive hExhausted =>
          rw [hPending] at hFind
          cases hFind
          have hOldLive := hRoots old hOldMem'
          have hSlotNe : old.token.slot ≠ target.token.slot := by
            exact Runtime.token_ne_slot_of_distinct_live_tokens hTokenNe hOldLive
              ⟨hAuth, ⟨hInBounds, hRemovedLive⟩⟩
          rcases hOldLive with ⟨hSession, ⟨hBounds, hSlot⟩⟩
          refine ⟨hSession, ⟨?_, ?_⟩⟩
          · rw [List.length_set]
            exact hBounds
          · dsimp
            rw [List.getElem_set_ne hSlotNe.symm]
            exact hSlot

theorem visibleRootsValid_after_updateStage
    {s : State} {runtime' : Runtime.State}
    {key : TopicKey} {stage : TopicStage} {runtimeId : Runtime.InitializerId}
    (hInv : s.VisibleTopicRootsValid)
    (hRuntime : Runtime.Step s.runtime (.publishTopic runtimeId) runtime') :
    ({ s with runtime := runtime', byKey := s.updateTopicStage key stage }).VisibleTopicRootsValid := by
  intro topic hMem
  dsimp [State.updateTopicStage] at hMem
  rcases List.mem_map.mp hMem with ⟨old, hOldMem, rfl⟩
  cases hRuntime
  by_cases hKey : (old.key == key) = true <;> simp [hKey]
  all_goals exact hInv old hOldMem

theorem Step.visibleTopicRootsValid_preserved
    {s s' : State} {e : Event}
    (hInv : s.VisibleTopicRootsValid)
    (hTokens : s.VisibleTokensUnique)
    (hStep : Step s e s') :
    s'.VisibleTopicRootsValid := by
  cases hStep with
  | beginPrepare hRuntime =>
      intro topic hMem
      cases hRuntime
      exact hInv topic hMem
  | endPrepare hRuntime =>
      intro topic hMem
      cases hRuntime
      exact hInv topic hMem
  | sealTopics hRuntime =>
      intro topic hMem
      cases hRuntime
      exact hInv topic hMem
  | beginLookup hRuntime =>
      intro topic hMem
      cases hRuntime with
      | beginLookup hReg => cases hReg; exact hInv topic hMem
  | endLookup hRuntime =>
      intro topic hMem
      cases hRuntime with
      | endLookup hReg => cases hReg; exact hInv topic hMem
  | beginInitializer hNoTopic hNoInitializer hNoRuntimeId hRuntime =>
      intro topic hMem
      cases hRuntime
      exact hInv topic hMem
  | publishVisibleFresh hInit hNoTopic hNoToken hRuntime hPending hRoot =>
      intro topic hMem
      simp only [List.mem_append, List.mem_singleton] at hMem
      cases hMem with
      | inl hOld =>
          exact visibleRootsValid_after_insertFresh hInv hRuntime topic hOld
      | inr hNew =>
          subst hNew
          exact hRoot
  | publishVisibleReuse hInit hNoTopic hNoToken hRuntime hPending hRoot =>
      intro topic hMem
      simp only [List.mem_append, List.mem_singleton] at hMem
      cases hMem with
      | inl hOld =>
          exact visibleRootsValid_after_insertReuse hInv hRuntime topic hOld
      | inr hNew =>
          subst hNew
          exact hRoot
  | commitPublication hInit hTopic hTopicKey hPending hRuntime =>
      exact visibleRootsValid_after_updateStage hInv hRuntime
  | rollbackVisibleReuse hInit hTopic hTopicKey hPending hRuntime =>
      have hTarget := mem_of_findTopic_some hTopic
      exact visibleRootsValid_after_rollbackReuse hInv hTokens hTarget
        hTopicKey hPending hRuntime
  | rollbackVisibleRetire hInit hTopic hTopicKey hPending hRuntime =>
      have hTarget := mem_of_findTopic_some hTopic
      exact visibleRootsValid_after_rollbackRetire hInv hTokens hTarget
        hTopicKey hPending hRuntime
  | finishInitializer hInit hReady hRuntime =>
      intro topic hMem
      cases hRuntime
      exact hInv topic hMem

end XlFnFormal.Handle.Topics
