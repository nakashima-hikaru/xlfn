import XlFnFormal.Handle.Topics.Invariant
import XlFnFormal.Handle.Runtime.Safety

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Topics

theorem same_key_has_at_most_one_initializer
    {s : State} {key : TopicKey} {left right : Initializer}
    (hInv : s.Invariant)
    (hLeft : left ∈ s.initializing)
    (hRight : right ∈ s.initializing)
    (hLeftKey : left.key = key)
    (hRightKey : right.key = key) :
    left = right := by
  by_cases hEq : left = right
  · exact hEq
  · exfalso
    have hRelation := pairwise_mem_ne_topics hInv.2.1 hLeft hRight hEq
    cases hRelation with
    | inl hNotEqual => exact hNotEqual (hLeftKey.trans hRightKey.symm)
    | inr hNotEqual => exact hNotEqual (hRightKey.trans hLeftKey.symm)

theorem same_key_has_at_most_one_visible_topic
    {s : State} {key : TopicKey} {left right : Topic}
    (hInv : s.Invariant)
    (hLeft : left ∈ s.byKey)
    (hRight : right ∈ s.byKey)
    (hLeftKey : left.key = key)
    (hRightKey : right.key = key) :
    left = right := by
  by_cases hEq : left = right
  · exact hEq
  · exfalso
    have hRelation := pairwise_mem_ne_topics hInv.2.2.2.1 hLeft hRight hEq
    cases hRelation with
    | inl hNotEqual => exact hNotEqual (hLeftKey.trans hRightKey.symm)
    | inr hNotEqual => exact hNotEqual (hRightKey.trans hLeftKey.symm)

theorem same_key_has_at_most_one_committed_topic
    {s : State} {key : TopicKey} {left right : Topic}
    (hInv : s.Invariant)
    (hLeft : left ∈ s.byKey)
    (hRight : right ∈ s.byKey)
    (hLeftStage : left.stage = .committed)
    (hRightStage : right.stage = .committed)
    (hLeftKey : left.key = key)
    (hRightKey : right.key = key) :
    left = right :=
  same_key_has_at_most_one_visible_topic hInv hLeft hRight hLeftKey hRightKey

theorem distinct_visible_topics_have_distinct_tokens
    {s : State} {left right : Topic}
    (hInv : s.Invariant)
    (hLeft : left ∈ s.byKey)
    (hRight : right ∈ s.byKey)
    (hNe : left ≠ right) :
    left.token ≠ right.token := by
  have hRelation := pairwise_mem_ne_topics hInv.2.2.2.2.1 hLeft hRight hNe
  cases hRelation with
  | inl hNotEqual => exact hNotEqual
  | inr hNotEqual => exact hNotEqual.symm

theorem distinct_committed_topics_have_distinct_tokens
    {s : State} {left right : Topic}
    (hInv : s.Invariant)
    (hLeft : left ∈ s.byKey)
    (hRight : right ∈ s.byKey)
    (hLeftStage : left.stage = .committed)
    (hRightStage : right.stage = .committed)
    (hNe : left ≠ right) :
    left.token ≠ right.token :=
  distinct_visible_topics_have_distinct_tokens hInv hLeft hRight hNe

theorem committed_topic_root_is_live
    {s : State} {topic : Topic}
    (hInv : s.Invariant)
    (hTopic : topic ∈ s.byKey)
    (hCommitted : topic.stage = .committed) :
    Runtime.TokenLive s.runtime.registry topic.token :=
  hInv.2.2.2.2.2.1 topic hTopic

theorem provisional_topic_has_pending_provenance
    {s : State} {topic : Topic}
    (hInv : s.Invariant)
    (hTopic : topic ∈ s.byKey)
    (hProvisional : topic.stage = .provisional) :
    ∃ init ∈ s.initializing,
      init.key = topic.key ∧
      s.runtime.findInitializer? init.runtimeId =
        some { id := init.runtimeId, stage := .pending topic.token } :=
  hInv.2.2.2.2.2.2 topic hTopic hProvisional

theorem publish_visible_fresh_exposes_pending_provenance
    {s s' : State} {key : TopicKey} {runtimeId : Runtime.InitializerId}
    (hStep : Step s (.publishVisibleFresh key runtimeId) s') :
    ∃ topic,
      topic ∈ s'.byKey ∧
      topic.key = key ∧
      topic.stage = .provisional ∧
      s'.runtime.findInitializer? runtimeId =
        some { id := runtimeId, stage := .pending topic.token } := by
  cases hStep with
  | publishVisibleFresh =>
      rename_i runtime' token hNoToken hRoot hNoTopic hRuntime hPending hInit
      refine ⟨{ key := key, token := token, stage := .provisional }, ?_, rfl, rfl, ?_⟩
      · simp
      · exact hPending

theorem commit_publication_resolves_initializer
    {s s' : State} {key : TopicKey} {runtimeId : Runtime.InitializerId}
    (hStep : Step s (.commitPublication key runtimeId) s') :
    ∃ topic,
      topic ∈ s'.byKey ∧
      topic.key = key ∧
      topic.stage = .committed ∧
      s'.runtime.findInitializer? runtimeId =
        some { id := runtimeId, stage := .resolved } := by
  cases hStep with
  | commitPublication =>
      rename_i runtime' source hTopic hTopicKey hPending hRuntime hInit
      have hTopicMem : { source with stage := .provisional } ∈ s.byKey :=
        mem_of_findTopic_some hTopic
      refine ⟨{ key := key, token := source.token, stage := .committed }, ?_, rfl, rfl, ?_⟩
      · dsimp [State.updateTopicStage]
        simp only [List.mem_map]
        refine ⟨{ source with stage := .provisional }, hTopicMem, ?_⟩
        simp [hTopicKey]
      · cases hRuntime
        dsimp [Runtime.State.findInitializer?]
        have hResolved := Runtime.updateInitializer_find hPending (stage := .resolved)
        exact hResolved

theorem rollback_visible_removes_topic_key
    {s s' : State} {key : TopicKey} {runtimeId : Runtime.InitializerId}
    {nextGeneration : Registry.Generation}
    (hStep : Step s (.rollbackVisibleReuse key runtimeId nextGeneration) s') :
    ∀ topic ∈ s'.byKey, topic.key ≠ key := by
  cases hStep with
  | rollbackVisibleReuse =>
      intro topic hMem
      dsimp [State.removeTopic] at hMem
      rcases List.mem_filter.mp hMem with ⟨_, hNe⟩
      intro hEq
      have hFalse : (topic.key != key) = false := by simp [hEq]
      rw [hFalse] at hNe
      contradiction

end XlFnFormal.Handle.Topics
