import XlFnFormal.Handle.Topics.Destruction

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Topics

open Registry (Generation Token nextGeneration?)

theorem detached_token_of_find
    {s : State} {token : Token} {detached : DetachedTopic}
    (hFind : s.findDetached? token = some detached) :
    detached.topic.token = token := by
  dsimp [State.findDetached?] at hFind
  have hPred : (detached.topic.token == token) = true := by
    exact List.find?_some
      (p := fun candidate : DetachedTopic => candidate.topic.token == token) hFind
  exact beq_iff_eq.mp hPred

theorem disconnect_detaches_visible_topic
    {s s' : State} {key : TopicKey} {owner : ExcelOwnerId}
    (hStep : DestructionStep s (.disconnectTopic key owner) s') :
    (∀ topic ∈ s'.byKey, topic.key ≠ key) ∧
    (∀ binding ∈ s'.byExcelOwner, binding.owner ≠ owner) := by
  cases hStep with
  | disconnectTopic hTopic hTopicKey hTopicOwner hBinding hNoDetached =>
      refine ⟨?_, ?_⟩
      · intro topic hMem
        dsimp [State.removeTopic] at hMem
        rcases List.mem_filter.mp hMem with ⟨_, hNe⟩
        intro hEq
        have hFalse : (topic.key != key) = false := by simp [hEq]
        rw [hFalse] at hNe
        contradiction
      · intro binding hMem
        dsimp [State.removeExcelOwner] at hMem
        rcases List.mem_filter.mp hMem with ⟨_, hNe⟩
        intro hEq
        have hFalse : (binding.owner != owner) = false := by simp [hEq]
        rw [hFalse] at hNe
        contradiction

theorem disconnect_removes_reverse_entry
    {s s' : State} {key : TopicKey} {owner : ExcelOwnerId}
    (hStep : DestructionStep s (.disconnectTopic key owner) s') :
    ∃ topic, topic ∈ s.byKey ∧ topic.key = key ∧
      ∀ entry ∈ s'.byRtdKey, entry.rtdKey ≠ topic.rtdKey := by
  cases hStep with
  | disconnectTopic hTopic hTopicKey hTopicOwner hBinding hNoDetached =>
      rename_i source
      refine ⟨source, mem_of_findTopic_some hTopic, hTopicKey, ?_⟩
      intro entry hMem
      dsimp [State.removeReverse] at hMem
      rcases List.mem_filter.mp hMem with ⟨_, hNe⟩
      intro hEq
      have hFalse : (entry.rtdKey != source.rtdKey) = false := by simp [hEq]
      rw [hFalse] at hNe
      contradiction

theorem disconnect_retains_root_until_drain
    {s s' : State} {key : TopicKey} {owner : ExcelOwnerId}
    (hInv : s.Invariant)
    (hStep : DestructionStep s (.disconnectTopic key owner) s') :
    ∃ detached ∈ s'.detached,
      detached.topic.key = key ∧
      Runtime.TokenLive s'.runtime.registry detached.topic.token := by
  cases hStep with
  | disconnectTopic hTopic hTopicKey hTopicOwner hBinding hNoDetached =>
      rename_i source
      have hSourceMem : source ∈ s.byKey := mem_of_findTopic_some hTopic
      have hRoot : Runtime.TokenLive s.runtime.registry source.token :=
        hInv.2.2.2.2.2.2.2.2.2.2.1 source hSourceMem
      refine ⟨{ topic := source }, ?_, ?_, ?_⟩
      · simp
      · simpa [hTopicKey]
      · exact hRoot

theorem drain_pending_reuse_removes_root
    {s s' : State} {token : Token} {runtimeId : Runtime.InitializerId}
    {nextGeneration : Generation}
    (hStep : DestructionStep s
      (.drainPendingReuse token runtimeId nextGeneration) s') :
    s'.findDetached? token = none ∧
    ¬ Runtime.TokenLive s'.runtime.registry token := by
  cases hStep with
  | drainPendingReuse hDetached hInit hPending hRuntime =>
      have hRemoved := Runtime.rollback_removes_pending_root_reuse hPending hRuntime
      refine ⟨?_, hRemoved.2⟩
      dsimp [State.findDetached?, State.removeDetached]
      simp

theorem drain_pending_retire_removes_root
    {s s' : State} {token : Token} {runtimeId : Runtime.InitializerId}
    (hStep : DestructionStep s
      (.drainPendingRetire token runtimeId) s') :
    s'.findDetached? token = none ∧
    ¬ Runtime.TokenLive s'.runtime.registry token := by
  cases hStep with
  | drainPendingRetire hDetached hInit hPending hRuntime =>
      have hRemoved := Runtime.rollback_removes_pending_root_retire hPending hRuntime
      refine ⟨?_, hRemoved.2⟩
      dsimp [State.findDetached?, State.removeDetached]
      simp

private theorem registry_remove_reuse_removes_token
    {registry registry' : Registry.State} {token : Token}
    {nextGeneration : Generation}
    (hStep : Registry.Step registry
      (.removeReuse token nextGeneration) registry') :
    ¬ Runtime.TokenLive registry' token := by
  intro hLive
  cases hStep with
  | removeReuse hAuth hInBounds hLiveBefore hNextGeneration =>
      rcases hLive with ⟨hSession, ⟨hBounds, hLiveAfter⟩⟩
      dsimp at hLiveAfter
      rw [List.getElem_set_self] at hLiveAfter
      contradiction

private theorem registry_remove_retire_removes_token
    {registry registry' : Registry.State} {token : Token}
    (hStep : Registry.Step registry (.removeRetire token) registry') :
    ¬ Runtime.TokenLive registry' token := by
  intro hLive
  cases hStep with
  | removeRetire hAuth hInBounds hLiveBefore hExhausted =>
      rcases hLive with ⟨hSession, ⟨hBounds, hLiveAfter⟩⟩
      dsimp at hLiveAfter
      rw [List.getElem_set_self] at hLiveAfter
      contradiction

theorem drain_published_reuse_removes_root
    {s s' : State} {token : Token} {nextGeneration : Generation}
    (hStep : DestructionStep s
      (.drainPublishedReuse token nextGeneration) s') :
    s'.findDetached? token = none ∧
    ¬ Runtime.TokenLive s'.runtime.registry token := by
  cases hStep with
  | drainPublishedReuse hDetached hPublished hNoPending hRegistry =>
      refine ⟨?_, ?_⟩
      · dsimp [State.findDetached?, State.removeDetached]
        simp
      · exact registry_remove_reuse_removes_token hRegistry

theorem drain_published_retire_removes_root
    {s s' : State} {token : Token}
    (hStep : DestructionStep s
      (.drainPublishedRetire token) s') :
    s'.findDetached? token = none ∧
    ¬ Runtime.TokenLive s'.runtime.registry token := by
  cases hStep with
  | drainPublishedRetire hDetached hPublished hNoPending hRegistry =>
      refine ⟨?_, ?_⟩
      · dsimp [State.findDetached?, State.removeDetached]
        simp
      · exact registry_remove_retire_removes_token hRegistry

theorem published_drain_rejects_pending_root
    {s s' : State} {token : Token}
    {event : DestructionEvent}
    (hPublishedEvent :
      (∃ nextGeneration, event = .drainPublishedReuse token nextGeneration) ∨
      event = .drainPublishedRetire token)
    (hStep : DestructionStep s event s') :
    ¬ ∃ init ∈ s.runtime.initializers,
      init.stage = .pending token := by
  rcases hPublishedEvent with ⟨nextGeneration, rfl⟩ | rfl
  · cases hStep with
    | drainPublishedReuse hDetached hPublished hNoPending hRegistry =>
        intro hPending
        rcases hPending with ⟨init, hMem, hStage⟩
        exact hNoPending init hMem hStage
  · cases hStep with
    | drainPublishedRetire hDetached hPublished hNoPending hRegistry =>
        intro hPending
        rcases hPending with ⟨init, hMem, hStage⟩
        exact hNoPending init hMem hStage

end XlFnFormal.Handle.Topics
