import XlFnFormal.Handle.Topics.Invariant

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
    have hRelation := pairwise_mem_ne_topics hInv.1 hLeft hRight hEq
    cases hRelation with
    | inl hNotEqual =>
        exact hNotEqual (hLeftKey.trans hRightKey.symm)
    | inr hNotEqual =>
        exact hNotEqual (hRightKey.trans hLeftKey.symm)

theorem same_key_has_at_most_one_committed_topic
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
    have hRelation := pairwise_mem_ne_topics hInv.2.1 hLeft hRight hEq
    cases hRelation with
    | inl hNotEqual =>
        exact hNotEqual (hLeftKey.trans hRightKey.symm)
    | inr hNotEqual =>
        exact hNotEqual (hRightKey.trans hLeftKey.symm)

theorem committed_topic_root_is_live
    {s : State} {topic : Topic}
    (hInv : s.Invariant)
    (hCommitted : topic ∈ s.byKey) :
    Registry.TokenLive s.registry topic.token :=
  hInv.2.2 topic hCommitted

theorem committed_topic_has_exactly_one_live_token
    {s : State} {topic : Topic}
    (hInv : s.Invariant)
    (hCommitted : topic ∈ s.byKey) :
    ∃ token,
      token = topic.token ∧
      Registry.TokenLive s.registry token ∧
      (∀ other, other = topic.token ∧ Registry.TokenLive s.registry other → other = token) := by
  refine ⟨topic.token, rfl, committed_topic_root_is_live hInv hCommitted, ?_⟩
  intro token hToken
  exact hToken.1

theorem publish_creates_one_committed_live_root
    {s s' : State} {key : TopicKey} {owner : OwnerId}
    {rtdKey : RtdKey} {token : Registry.Token}
    (hInv : s.Invariant)
    (hStep : Step s (.publish key owner rtdKey token) s') :
    ∃ topic,
      topic ∈ s'.byKey ∧
      topic.key = key ∧
      topic.token = token ∧
      Registry.TokenLive s'.registry topic.token := by
  cases hStep with
  | publish hFind hNoTopic hRoot =>
      refine ⟨{ key := key, rtdKey := rtdKey, token := token }, ?_, rfl, rfl, ?_⟩
      · simp
      · exact hRoot

end XlFnFormal.Handle.Topics
