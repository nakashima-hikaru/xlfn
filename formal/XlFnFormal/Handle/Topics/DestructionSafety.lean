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

private theorem detached_tokens_unique_after_remove
    {s : State} {token : Token}
    (hInv : s.DetachedTokensUnique) :
    ({ s with detached := s.removeDetached token }).DetachedTokensUnique := by
  dsimp [State.DetachedTokensUnique, State.removeDetached] at hInv ⊢
  exact pairwise_filter_topics (fun detached => detached.topic.token != token) hInv

private theorem detached_disjoint_after_remove
    {s : State} {token : Token}
    (hInv : s.DetachedTokensDisjointVisible) :
    ({ s with detached := s.removeDetached token }).DetachedTokensDisjointVisible := by
  intro detached hDetached topic hTopic
  exact hInv detached (mem_of_mem_filter_topics hDetached) topic hTopic

private theorem detached_provisional_after_remove
    {s : State} {token : Token}
    (hInv : s.DetachedProvisionalRootsHavePendingOwners) :
    ({ s with detached := s.removeDetached token }).DetachedProvisionalRootsHavePendingOwners := by
  intro detached hDetached hStage
  exact hInv detached (mem_of_mem_filter_topics hDetached) hStage

private theorem registry_remove_reuse_preserves_other_live
    {registry registry' : Registry.State} {kept removed : Token}
    {nextGeneration : Generation}
    (hKept : Runtime.TokenLive registry kept)
    (hNe : kept ≠ removed)
    (hStep : Registry.Step registry
      (.removeReuse removed nextGeneration) registry') :
    Runtime.TokenLive registry' kept := by
  cases hStep with
  | removeReuse hAuth hInBounds hRemovedLive hNextGeneration =>
      have hRemoved : Runtime.TokenLive registry removed :=
        ⟨hAuth, ⟨hInBounds, hRemovedLive⟩⟩
      have hSlotNe := Runtime.token_ne_slot_of_distinct_live_tokens
        hNe hKept hRemoved
      rcases hKept with ⟨hSession, ⟨hBounds, hSlot⟩⟩
      refine ⟨hSession, ⟨?_, ?_⟩⟩
      · rw [List.length_set]
        exact hBounds
      · dsimp
        rw [List.getElem_set_ne hSlotNe.symm]
        exact hSlot

private theorem registry_remove_retire_preserves_other_live
    {registry registry' : Registry.State} {kept removed : Token}
    (hKept : Runtime.TokenLive registry kept)
    (hNe : kept ≠ removed)
    (hStep : Registry.Step registry
      (.removeRetire removed) registry') :
    Runtime.TokenLive registry' kept := by
  cases hStep with
  | removeRetire hAuth hInBounds hRemovedLive hExhausted =>
      have hRemoved : Runtime.TokenLive registry removed :=
        ⟨hAuth, ⟨hInBounds, hRemovedLive⟩⟩
      have hSlotNe := Runtime.token_ne_slot_of_distinct_live_tokens
        hNe hKept hRemoved
      rcases hKept with ⟨hSession, ⟨hBounds, hSlot⟩⟩
      refine ⟨hSession, ⟨?_, ?_⟩⟩
      · rw [List.length_set]
        exact hBounds
      · dsimp
        rw [List.getElem_set_ne hSlotNe.symm]
        exact hSlot

private theorem runtime_invariant_after_published_reuse
    {runtime : Runtime.State} {registry' : Registry.State} {token : Token}
    {nextGeneration : Generation}
    (hInv : Runtime.RuntimeInvariant runtime)
    (hNoPending : ∀ init ∈ runtime.initializers,
      init.stage ≠ .pending token)
    (hStep : Registry.Step runtime.registry
      (.removeReuse token nextGeneration) registry') :
    Runtime.RuntimeInvariant { runtime with registry := registry' } := by
  rcases hInv with ⟨hPhase, hOp, hIds, hToks, hRoots⟩
  refine ⟨?_, hOp, hIds, hToks, ?_⟩
  · cases hP : runtime.phase with
    | «open» =>
        cases hStep
        simpa [Runtime.PhaseInvariant, hP] using hPhase
    | drainingPrepares =>
        cases hStep
        simpa [Runtime.PhaseInvariant, hP] using hPhase
    | registryClosed =>
        have hFields := Runtime.phaseInvariant_registryClosed_fields hPhase hP
        cases hStep with
        | removeReuse hAuth hInBounds hLive hNextGeneration =>
            exact (Registry.noLiveSlots_contradiction hFields.2.2.2 hLive).elim
    | closed =>
        have hFields := Runtime.phaseInvariant_closed_fields hPhase hP
        cases hStep with
        | removeReuse hAuth hInBounds hLive hNextGeneration =>
            exact (Registry.noLiveSlots_contradiction hFields.2.2.2.2 hLive).elim
  · intro init hInit
    change init ∈ runtime.initializers at hInit
    cases hStage : init.stage with
    | pending kept =>
        have hKept : Runtime.TokenLive runtime.registry kept := by
          simpa [hStage] using hRoots init hInit
        have hNe : kept ≠ token := by
          intro hEq
          apply hNoPending init hInit
          simpa [hStage, hEq]
        exact registry_remove_reuse_preserves_other_live hKept hNe hStep
    | beforeInsert => trivial
    | resolved => trivial

private theorem runtime_invariant_after_published_retire
    {runtime : Runtime.State} {registry' : Registry.State} {token : Token}
    (hInv : Runtime.RuntimeInvariant runtime)
    (hNoPending : ∀ init ∈ runtime.initializers,
      init.stage ≠ .pending token)
    (hStep : Registry.Step runtime.registry
      (.removeRetire token) registry') :
    Runtime.RuntimeInvariant { runtime with registry := registry' } := by
  rcases hInv with ⟨hPhase, hOp, hIds, hToks, hRoots⟩
  refine ⟨?_, hOp, hIds, hToks, ?_⟩
  · cases hP : runtime.phase with
    | «open» =>
        cases hStep
        simpa [Runtime.PhaseInvariant, hP] using hPhase
    | drainingPrepares =>
        cases hStep
        simpa [Runtime.PhaseInvariant, hP] using hPhase
    | registryClosed =>
        have hFields := Runtime.phaseInvariant_registryClosed_fields hPhase hP
        cases hStep with
        | removeRetire hAuth hInBounds hLive hExhausted =>
            exact (Registry.noLiveSlots_contradiction hFields.2.2.2 hLive).elim
    | closed =>
        have hFields := Runtime.phaseInvariant_closed_fields hPhase hP
        cases hStep with
        | removeRetire hAuth hInBounds hLive hExhausted =>
            exact (Registry.noLiveSlots_contradiction hFields.2.2.2.2 hLive).elim
  · intro init hInit
    change init ∈ runtime.initializers at hInit
    cases hStage : init.stage with
    | pending kept =>
        have hKept : Runtime.TokenLive runtime.registry kept := by
          simpa [hStage] using hRoots init hInit
        have hNe : kept ≠ token := by
          intro hEq
          apply hNoPending init hInit
          simpa [hStage, hEq]
        exact registry_remove_retire_preserves_other_live hKept hNe hStep
    | beforeInsert => trivial
    | resolved => trivial

private theorem visible_roots_after_published_reuse
    {s : State} {registry' : Registry.State} {token : Token}
    {nextGeneration : Generation}
    (hRoots : s.VisibleTopicRootsValid)
    (hNoToken : ∀ topic ∈ s.byKey, topic.token ≠ token)
    (hStep : Registry.Step s.runtime.registry
      (.removeReuse token nextGeneration) registry') :
    ∀ topic ∈ s.byKey,
      Runtime.TokenLive { s.runtime with registry := registry' }.registry topic.token := by
  intro topic hTopic
  exact registry_remove_reuse_preserves_other_live (hRoots topic hTopic)
    (hNoToken topic hTopic) hStep

private theorem visible_roots_after_published_retire
    {s : State} {registry' : Registry.State} {token : Token}
    (hRoots : s.VisibleTopicRootsValid)
    (hNoToken : ∀ topic ∈ s.byKey, topic.token ≠ token)
    (hStep : Registry.Step s.runtime.registry
      (.removeRetire token) registry') :
    ∀ topic ∈ s.byKey,
      Runtime.TokenLive { s.runtime with registry := registry' }.registry topic.token := by
  intro topic hTopic
  exact registry_remove_retire_preserves_other_live (hRoots topic hTopic)
    (hNoToken topic hTopic) hStep

private theorem detached_roots_after_pending_reuse
    {s : State} {runtime' : Runtime.State} {detached : DetachedTopic}
    {token : Token} {runtimeId : Runtime.InitializerId}
    {nextGeneration : Generation}
    (hRoots : s.DetachedRootsValid)
    (hDetached : s.findDetached? token = some detached)
    (hPending : s.runtime.findInitializer? runtimeId =
      some { id := runtimeId, stage := .pending token })
    (hStep : Runtime.Step s.runtime
      (.rollbackPendingReuse runtimeId nextGeneration) runtime') :
    ∀ old ∈ s.removeDetached token,
      Runtime.TokenLive runtime'.registry old.topic.token := by
  intro old hOld
  have hOldMem := mem_of_mem_filter_topics hOld
  have hTokenNe : old.topic.token ≠ token := by
    dsimp [State.removeDetached] at hOld
    rcases List.mem_filter.mp hOld with ⟨_, hNe⟩
    intro hEq
    have hFalse : (old.topic.token != token) = false := by simp [hEq]
    rw [hFalse] at hNe
    contradiction
  cases hStep with
  | rollbackPendingReuse hFind hRegistry =>
      rw [hPending] at hFind
      cases hFind
      exact registry_remove_reuse_preserves_other_live
        (hRoots old hOldMem) hTokenNe hRegistry

private theorem detached_roots_after_pending_retire
    {s : State} {runtime' : Runtime.State} {detached : DetachedTopic}
    {token : Token} {runtimeId : Runtime.InitializerId}
    (hRoots : s.DetachedRootsValid)
    (hDetached : s.findDetached? token = some detached)
    (hPending : s.runtime.findInitializer? runtimeId =
      some { id := runtimeId, stage := .pending token })
    (hStep : Runtime.Step s.runtime
      (.rollbackPendingRetire runtimeId) runtime') :
    ∀ old ∈ s.removeDetached token,
      Runtime.TokenLive runtime'.registry old.topic.token := by
  intro old hOld
  have hOldMem := mem_of_mem_filter_topics hOld
  have hTokenNe : old.topic.token ≠ token := by
    dsimp [State.removeDetached] at hOld
    rcases List.mem_filter.mp hOld with ⟨_, hNe⟩
    intro hEq
    have hFalse : (old.topic.token != token) = false := by simp [hEq]
    rw [hFalse] at hNe
    contradiction
  cases hStep with
  | rollbackPendingRetire hFind hRegistry =>
      rw [hPending] at hFind
      cases hFind
      exact registry_remove_retire_preserves_other_live
        (hRoots old hOldMem) hTokenNe hRegistry

private theorem detached_roots_after_published_reuse
    {s : State} {registry' : Registry.State} {detached : DetachedTopic}
    {token : Token} {nextGeneration : Generation}
    (hRoots : s.DetachedRootsValid)
    (hStep : Registry.Step s.runtime.registry
      (.removeReuse token nextGeneration) registry') :
    ∀ old ∈ s.removeDetached token,
      Runtime.TokenLive { s.runtime with registry := registry' }.registry old.topic.token := by
  intro old hOld
  have hOldMem := mem_of_mem_filter_topics hOld
  have hTokenNe : old.topic.token ≠ token := by
    dsimp [State.removeDetached] at hOld
    rcases List.mem_filter.mp hOld with ⟨_, hNe⟩
    intro hEq
    have hFalse : (old.topic.token != token) = false := by simp [hEq]
    rw [hFalse] at hNe
    contradiction
  exact registry_remove_reuse_preserves_other_live
    (hRoots old hOldMem) hTokenNe hStep

private theorem detached_roots_after_published_retire
    {s : State} {registry' : Registry.State} {detached : DetachedTopic}
    {token : Token}
    (hRoots : s.DetachedRootsValid)
    (hStep : Registry.Step s.runtime.registry
      (.removeRetire token) registry') :
    ∀ old ∈ s.removeDetached token,
      Runtime.TokenLive { s.runtime with registry := registry' }.registry old.topic.token := by
  intro old hOld
  have hOldMem := mem_of_mem_filter_topics hOld
  have hTokenNe : old.topic.token ≠ token := by
    dsimp [State.removeDetached] at hOld
    rcases List.mem_filter.mp hOld with ⟨_, hNe⟩
    intro hEq
    have hFalse : (old.topic.token != token) = false := by simp [hEq]
    rw [hFalse] at hNe
    contradiction
  exact registry_remove_retire_preserves_other_live
    (hRoots old hOldMem) hTokenNe hStep

private theorem reverse_sound_after_disconnect
    {s : State} {source : Topic} {key : TopicKey}
    (hSound : s.ReverseMapSound)
    (hKeys : s.VisibleKeysUnique)
    (hSourceMem : source ∈ s.byKey)
    (hSourceKey : source.key = key) :
    ({ s with
        byKey := s.removeTopic key
        byRtdKey := s.removeReverse source.rtdKey }).ReverseMapSound := by
  intro entry hEntry
  change entry ∈ s.removeReverse source.rtdKey at hEntry
  rcases List.mem_filter.mp hEntry with ⟨hEntryMem, hEntryNe⟩
  have hEntryRtdNe : entry.rtdKey ≠ source.rtdKey := by
    intro hEq
    have hFalse : (entry.rtdKey != source.rtdKey) = false := by simp [hEq]
    rw [hFalse] at hEntryNe
    contradiction
  rcases hSound entry hEntryMem with ⟨old, hOldMem, hOldKey, hOldRtd⟩
  have hOldKeyNe : old.key ≠ key := by
    intro hEq
    have hOldEq := topic_eq_of_same_key hKeys hOldMem hSourceMem hEq hSourceKey
    apply hEntryRtdNe
    calc
      entry.rtdKey = old.rtdKey := hOldRtd.symm
      _ = source.rtdKey := by simpa [hOldEq]
  refine ⟨old, ?_, hOldKey, hOldRtd⟩
  apply List.mem_filter.mpr
  exact ⟨hOldMem, by simp [hOldKeyNe]⟩

private theorem reverse_complete_after_disconnect
    {s : State} {source : Topic} {key : TopicKey}
    (hComplete : s.ReverseMapComplete)
    (hRtdKeys : s.RtdKeysUnique)
    (hSourceMem : source ∈ s.byKey)
    (hSourceKey : source.key = key) :
    ({ s with
        byKey := s.removeTopic key
        byRtdKey := s.removeReverse source.rtdKey }).ReverseMapComplete := by
  intro topic hTopic
  change topic ∈ s.removeTopic key at hTopic
  have hTopicMem := mem_of_mem_filter_topics hTopic
  have hTopicKeyNe : topic.key ≠ key := by
    dsimp [State.removeTopic] at hTopic
    rcases List.mem_filter.mp hTopic with ⟨_, hNe⟩
    intro hEq
    have hFalse : (topic.key != key) = false := by simp [hEq]
    rw [hFalse] at hNe
    contradiction
  have hTopicNe : topic ≠ source := by
    intro hEq
    apply hTopicKeyNe
    simpa [hEq] using hSourceKey
  have hRtdNe := distinct_topics_have_rtd_keys hRtdKeys
    hTopicMem hSourceMem hTopicNe
  rcases hComplete topic hTopicMem with ⟨entry, hEntryMem, hEntryKey, hEntryRtd⟩
  refine ⟨entry, ?_, hEntryKey, hEntryRtd⟩
  apply List.mem_filter.mpr
  have hEntryRtdNe : entry.rtdKey ≠ source.rtdKey := by
    intro hEq
    apply hRtdNe
    exact hEntryRtd.symm.trans hEq
  have hEntryNe : (entry.rtdKey != source.rtdKey) = true := by
    simp [hEntryRtdNe]
  refine ⟨hEntryMem, hEntryNe⟩

private theorem provisional_roots_after_pending_drain
    {s : State} {runtime' : Runtime.State} {detached : DetachedTopic}
    {token : Token} {runtimeId : Runtime.InitializerId}
    {nextGeneration : Generation}
    (hInv : s.Invariant)
    (hKeys : s.InitializingKeysUnique)
    (hIds : s.InitializerIdsUnique)
    (hProv : s.ProvisionalTopicsHavePendingRoots)
    (hDisjoint : s.DetachedTokensDisjointVisible)
    (hDetached : s.findDetached? token = some detached)
    (hInit : s.findInitializing? detached.topic.key =
      some { runtimeId := runtimeId, key := detached.topic.key })
    (hPending : s.runtime.findInitializer? runtimeId =
      some { id := runtimeId, stage := .pending token })
    (hStep : Runtime.Step s.runtime
      (.rollbackPendingReuse runtimeId nextGeneration) runtime') :
    ∀ topic ∈ s.byKey, topic.stage = .provisional →
      ∃ init ∈ s.initializing,
        init.key = topic.key ∧
        runtime'.findInitializer? init.runtimeId =
          some { id := init.runtimeId, stage := .pending topic.token } := by
  have hTopicKeyNe : ∀ topic ∈ s.byKey, topic.stage = .provisional →
      topic.key ≠ detached.topic.key := by
    intro topic hTopic hStage hEq
    rcases provisional_topic_has_pending_provenance hInv hTopic hStage with
      ⟨topicInit, hTopicInit, hTopicInitKey, hTopicPending⟩
    have hDetachedInit :
        ({ runtimeId := runtimeId, key := detached.topic.key } : Initializer) ∈
          s.initializing := mem_of_findInitializing_some hInit
    have hInitEq := same_key_has_at_most_one_initializer hInv
      hTopicInit hDetachedInit (hTopicInitKey.trans hEq) rfl
    have hTopicPending' : s.runtime.findInitializer? runtimeId =
        some { id := runtimeId, stage := .pending topic.token } := by
      simpa [hInitEq] using hTopicPending
    have hStageEq := congrArg Runtime.Initializer.stage
      (Option.some.inj (hTopicPending'.symm.trans hPending))
    have hTokenEq : topic.token = token := by
      injection hStageEq with hTokenEq
    have hDetachedMem := mem_of_findDetached_some hDetached
    have hTokenNe := hDisjoint detached hDetachedMem topic hTopic
    exact hTokenNe (by simpa [detached_token_of_find hDetached, hTokenEq])
  cases hStep with
  | rollbackPendingReuse hFind hRegistry =>
      rw [hPending] at hFind
      cases hFind
      exact provisionalTopics_after_runtime_update hKeys hIds hProv hInit
        hTopicKeyNe rfl

private theorem detached_provisional_after_pending_drain
    {s : State} {runtime' : Runtime.State} {detached : DetachedTopic}
    {token : Token} {runtimeId : Runtime.InitializerId}
    {nextGeneration : Generation}
    (hProv : s.DetachedProvisionalRootsHavePendingOwners)
    (hDetached : s.findDetached? token = some detached)
    (hPending : s.runtime.findInitializer? runtimeId =
      some { id := runtimeId, stage := .pending token })
    (hStep : Runtime.Step s.runtime
      (.rollbackPendingReuse runtimeId nextGeneration) runtime') :
    ({ s with
        runtime := runtime'
        detached := s.removeDetached token }).DetachedProvisionalRootsHavePendingOwners := by
  intro old hOld hStage
  have hOldMem := mem_of_mem_filter_topics hOld
  rcases hProv old hOldMem hStage with
    ⟨init, hInitMem, hInitKey, hInitPending⟩
  have hIdNe : init.runtimeId ≠ runtimeId := by
    intro hEq
    have hInitPending' : s.runtime.findInitializer? runtimeId =
        some { id := runtimeId, stage := .pending old.topic.token } := by
      simpa [hEq] using hInitPending
    have hPendingEq := Option.some.inj (hInitPending'.symm.trans hPending)
    have hStageEq := congrArg Runtime.Initializer.stage hPendingEq
    have hTokenEq : old.topic.token = token := by
      injection hStageEq with hTokenEq
    dsimp [State.removeDetached] at hOld
    rcases List.mem_filter.mp hOld with ⟨_, hNe⟩
    have hFalse : (old.topic.token != token) = false := by
      simp [hTokenEq]
    rw [hFalse] at hNe
    contradiction
  cases hStep with
  | rollbackPendingReuse hFind hRegistry =>
      refine ⟨init, hInitMem, hInitKey, ?_⟩
      dsimp [Runtime.State.findInitializer?]
      exact runtime_find_update_ne hInitPending hIdNe

private theorem provisional_roots_after_pending_retire
    {s : State} {runtime' : Runtime.State} {detached : DetachedTopic}
    {token : Token} {runtimeId : Runtime.InitializerId}
    (hInv : s.Invariant)
    (hKeys : s.InitializingKeysUnique)
    (hIds : s.InitializerIdsUnique)
    (hProv : s.ProvisionalTopicsHavePendingRoots)
    (hDisjoint : s.DetachedTokensDisjointVisible)
    (hDetached : s.findDetached? token = some detached)
    (hInit : s.findInitializing? detached.topic.key =
      some { runtimeId := runtimeId, key := detached.topic.key })
    (hPending : s.runtime.findInitializer? runtimeId =
      some { id := runtimeId, stage := .pending token })
    (hStep : Runtime.Step s.runtime
      (.rollbackPendingRetire runtimeId) runtime') :
    ∀ topic ∈ s.byKey, topic.stage = .provisional →
      ∃ init ∈ s.initializing,
        init.key = topic.key ∧
        runtime'.findInitializer? init.runtimeId =
          some { id := init.runtimeId, stage := .pending topic.token } := by
  have hTopicKeyNe : ∀ topic ∈ s.byKey, topic.stage = .provisional →
      topic.key ≠ detached.topic.key := by
    intro topic hTopic hStage hEq
    rcases provisional_topic_has_pending_provenance hInv hTopic hStage with
      ⟨topicInit, hTopicInit, hTopicInitKey, hTopicPending⟩
    have hDetachedInit :
        ({ runtimeId := runtimeId, key := detached.topic.key } : Initializer) ∈
          s.initializing := mem_of_findInitializing_some hInit
    have hInitEq := same_key_has_at_most_one_initializer hInv
      hTopicInit hDetachedInit (hTopicInitKey.trans hEq) rfl
    have hTopicPending' : s.runtime.findInitializer? runtimeId =
        some { id := runtimeId, stage := .pending topic.token } := by
      simpa [hInitEq] using hTopicPending
    have hStageEq := congrArg Runtime.Initializer.stage
      (Option.some.inj (hTopicPending'.symm.trans hPending))
    have hTokenEq : topic.token = token := by
      injection hStageEq with hTokenEq
    have hDetachedMem := mem_of_findDetached_some hDetached
    have hTokenNe := hDisjoint detached hDetachedMem topic hTopic
    exact hTokenNe (by simpa [detached_token_of_find hDetached, hTokenEq])
  cases hStep with
  | rollbackPendingRetire hFind hRegistry =>
      rw [hPending] at hFind
      cases hFind
      exact provisionalTopics_after_runtime_update hKeys hIds hProv hInit
        hTopicKeyNe rfl

private theorem detached_provisional_after_pending_retire
    {s : State} {runtime' : Runtime.State} {detached : DetachedTopic}
    {token : Token} {runtimeId : Runtime.InitializerId}
    (hProv : s.DetachedProvisionalRootsHavePendingOwners)
    (hDetached : s.findDetached? token = some detached)
    (hPending : s.runtime.findInitializer? runtimeId =
      some { id := runtimeId, stage := .pending token })
    (hStep : Runtime.Step s.runtime
      (.rollbackPendingRetire runtimeId) runtime') :
    ({ s with
        runtime := runtime'
        detached := s.removeDetached token }).DetachedProvisionalRootsHavePendingOwners := by
  intro old hOld hStage
  have hOldMem := mem_of_mem_filter_topics hOld
  rcases hProv old hOldMem hStage with
    ⟨init, hInitMem, hInitKey, hInitPending⟩
  have hIdNe : init.runtimeId ≠ runtimeId := by
    intro hEq
    have hInitPending' : s.runtime.findInitializer? runtimeId =
        some { id := runtimeId, stage := .pending old.topic.token } := by
      simpa [hEq] using hInitPending
    have hPendingEq := Option.some.inj (hInitPending'.symm.trans hPending)
    have hStageEq := congrArg Runtime.Initializer.stage hPendingEq
    have hTokenEq : old.topic.token = token := by
      injection hStageEq with hTokenEq
    dsimp [State.removeDetached] at hOld
    rcases List.mem_filter.mp hOld with ⟨_, hNe⟩
    have hFalse : (old.topic.token != token) = false := by
      simp [hTokenEq]
    rw [hFalse] at hNe
    contradiction
  cases hStep with
  | rollbackPendingRetire hFind hRegistry =>
      refine ⟨init, hInitMem, hInitKey, ?_⟩
      dsimp [Runtime.State.findInitializer?]
      exact runtime_find_update_ne hInitPending hIdNe

private theorem detached_disjoint_after_disconnect
    {s : State} {source : Topic} {key : TopicKey}
    (hInv : s.Invariant)
    (hDisjoint : s.DetachedTokensDisjointVisible)
    (hSourceMem : source ∈ s.byKey)
    (hSourceKey : source.key = key) :
    State.DetachedTokensDisjointVisible
      { s with
          byKey := s.removeTopic key
          detached := s.detached ++ [{ topic := source }] } := by
  intro detached hDetached topic hTopic
  simp only [List.mem_append, List.mem_singleton] at hDetached
  cases hDetached with
  | inl hOld =>
      exact hDisjoint detached hOld topic
        (mem_of_mem_filter_topics hTopic)
  | inr hNew =>
      subst hNew
      have hTopicMem := mem_of_mem_filter_topics hTopic
      have hTopicKeyNe : topic.key ≠ key := by
        dsimp [State.removeTopic] at hTopic
        rcases List.mem_filter.mp hTopic with ⟨_, hNe⟩
        intro hEq
        have hFalse : (topic.key != key) = false := by simp [hEq]
        rw [hFalse] at hNe
        contradiction
      have hSourceNe : source ≠ topic := by
        intro hEq
        apply hTopicKeyNe
        simpa [hEq] using hSourceKey
      exact distinct_visible_topics_have_distinct_tokens hInv
        hSourceMem hTopicMem hSourceNe

private theorem initializers_backed_after_rollback_reuse
    {s : State} {runtime' : Runtime.State}
    {runtimeId : Runtime.InitializerId} {nextGeneration : Generation}
    (hBacked : s.InitializersBackedByRuntime)
    (hStep : Runtime.Step s.runtime
      (.rollbackPendingReuse runtimeId nextGeneration) runtime') :
    ({ s with runtime := runtime' }).InitializersBackedByRuntime := by
  intro init hMem
  rcases hBacked init hMem with ⟨runtimeInit, hRuntimeMem, hId⟩
  cases hStep with
  | rollbackPendingReuse hFind hRegistry =>
      rcases runtime_mem_updateInitializer_same_id hRuntimeMem with
        ⟨updated, hUpdatedMem, hUpdatedId⟩
      exact ⟨updated, hUpdatedMem, hUpdatedId.trans hId⟩

private theorem initializers_backed_after_rollback_retire
    {s : State} {runtime' : Runtime.State}
    {runtimeId : Runtime.InitializerId}
    (hBacked : s.InitializersBackedByRuntime)
    (hStep : Runtime.Step s.runtime
      (.rollbackPendingRetire runtimeId) runtime') :
    ({ s with runtime := runtime' }).InitializersBackedByRuntime := by
  intro init hMem
  rcases hBacked init hMem with ⟨runtimeInit, hRuntimeMem, hId⟩
  cases hStep with
  | rollbackPendingRetire hFind hRegistry =>
      rcases runtime_mem_updateInitializer_same_id hRuntimeMem with
        ⟨updated, hUpdatedMem, hUpdatedId⟩
      exact ⟨updated, hUpdatedMem, hUpdatedId.trans hId⟩

theorem DestructionStep.invariant_preserved
    {s s' : State} {event : DestructionEvent}
    (hInv : s.Invariant)
    (hStep : DestructionStep s event s') :
    s'.Invariant := by
  have hFull := hInv
  rcases hInv with
    ⟨hRuntime, hKeys, hIds, hBacked, hVisibleKeys, hVisibleTokens,
      hRtdKeys, hReverseRtdKeys, hReverseSound, hReverseComplete, hRoots,
      hProv, hExcel, hGeneration⟩
  rcases hExcel with
    ⟨hExcelSound, hExcelComplete, hExcelOwners, hExcelBindings, hExcelCommit⟩
  rcases hGeneration with ⟨hGeneration, hDestruction⟩
  cases hStep with
  | disconnectTopic hTopic hTopicKey hTopicOwner hBinding hNoDetached =>
      rename_i source key owner
      have hSourceMem : source ∈ s.byKey := mem_of_findTopic_some hTopic
      have hExcelSound' := excelOwnerMapSound_after_withdrawVisible
        hExcelSound hVisibleKeys hTopic hTopicKey
      have hExcelComplete' := excelOwnerMapComplete_after_withdrawVisible
        hExcelComplete hExcelOwners hVisibleKeys hTopic hTopicKey
      have hExcelOwners' := excelOwnersUnique_after_withdrawVisible
        (key := key) hExcelOwners
      have hExcelBindings' := excelBindingOwnersUnique_after_rollbackConnection
        (s := s) (owner := owner) hExcelBindings
      have hExcelCommit' := excelCommitConsistent_after_withdrawVisible
        (key := key) hExcelCommit
      refine ⟨
        hRuntime,
        ?_,
        hIds,
        hBacked,
        ?_,
        ?_,
        ?_,
        ?_,
        ?_,
        ?_,
        ?_,
        ?_,
        ?_,
        ?_,
        ?_⟩
      · dsimp [State.InitializingKeysUnique]
        exact hKeys
      · dsimp [State.VisibleKeysUnique, State.removeTopic]
        exact pairwise_filter_topics (fun topic => topic.key != key) hVisibleKeys
      · dsimp [State.VisibleTokensUnique, State.removeTopic]
        exact pairwise_filter_topics (fun topic => topic.key != key) hVisibleTokens
      · dsimp [State.RtdKeysUnique, State.removeTopic]
        exact pairwise_filter_topics (fun topic => topic.key != key) hRtdKeys
      · dsimp [State.ReverseRtdKeysUnique, State.removeReverse]
        exact pairwise_filter_topics
          (fun entry => entry.rtdKey != source.rtdKey) hReverseRtdKeys
      · exact reverse_sound_after_disconnect hReverseSound hVisibleKeys
          hSourceMem hTopicKey
      · exact reverse_complete_after_disconnect hReverseComplete hRtdKeys
          hSourceMem hTopicKey
      · intro topic hMem
        exact hRoots topic (mem_of_mem_filter_topics hMem)
      · intro topic hMem hStage
        exact hProv topic (mem_of_mem_filter_topics hMem) hStage
      · refine ⟨?_, ?_, ?_, ?_, ?_⟩
        · simpa [State.ExcelOwnerMapSound, hTopicOwner] using hExcelSound'
        · simpa [State.ExcelOwnerMapComplete, hTopicOwner] using hExcelComplete'
        · exact hExcelOwners'
        · simpa [State.ExcelBindingOwnersUnique, hTopicOwner] using hExcelBindings'
        · exact hExcelCommit'
      · exact excelOwnerGenerationConsistent_after_removeTopic hGeneration
      · refine ⟨?_, ?_, ?_, ?_⟩
        · dsimp [State.DetachedTokensUnique]
          apply pairwise_append_singleton_topics hDestruction.1
          intro detached hDetachedMem
          exact no_detached_member hNoDetached hDetachedMem
        · exact detached_disjoint_after_disconnect hFull hDestruction.2.1
            hSourceMem hTopicKey
        · intro detached hDetachedMem
          simp only [List.mem_append, List.mem_singleton] at hDetachedMem
          cases hDetachedMem with
          | inl hOld => exact hDestruction.2.2.1 detached hOld
          | inr hNew =>
              subst hNew
              exact hRoots source hSourceMem
        · intro detached hDetachedMem hStage
          simp only [List.mem_append, List.mem_singleton] at hDetachedMem
          cases hDetachedMem with
          | inl hOld => exact hDestruction.2.2.2 detached hOld hStage
          | inr hNew =>
              subst hNew
              exact hProv source hSourceMem hStage
  | drainPendingReuse hDetached hInit hPending hRuntimeStep =>
      rename_i detached token runtimeId nextGeneration
      have hRuntime' := Runtime.Step.runtimeInvariant_preserved hRuntime hRuntimeStep
      have hVisibleNoToken : ∀ topic ∈ s.byKey, topic.token ≠ token := by
        intro topic hTopic
        have hDetachedMem := mem_of_findDetached_some hDetached
        have hDisjoint := hDestruction.2.1 detached hDetachedMem topic hTopic
        intro hEq
        apply hDisjoint
        calc
          detached.topic.token = token := detached_token_of_find hDetached
          _ = topic.token := hEq.symm
      have hVisibleRoots' := visibleRootsValid_after_rollbackReuse
        hRoots hVisibleNoToken hPending hRuntimeStep
      have hProv' := provisional_roots_after_pending_drain hFull hKeys hIds hProv
        hDestruction.2.1 hDetached hInit hPending hRuntimeStep
      have hBacked' := initializers_backed_after_rollback_reuse hBacked hRuntimeStep
      refine ⟨
        hRuntime',
        hKeys,
        hIds,
        hBacked',
        hVisibleKeys,
        hVisibleTokens,
        hRtdKeys,
        hReverseRtdKeys,
        hReverseSound,
        hReverseComplete,
        hVisibleRoots',
        hProv',
        ⟨hExcelSound, hExcelComplete, hExcelOwners, hExcelBindings, hExcelCommit⟩,
        hGeneration,
        ⟨
          detached_tokens_unique_after_remove hDestruction.1,
          detached_disjoint_after_remove hDestruction.2.1,
          detached_roots_after_pending_reuse hDestruction.2.2.1 hDetached
            hPending hRuntimeStep,
          detached_provisional_after_pending_drain hDestruction.2.2.2 hDetached
            hPending hRuntimeStep⟩⟩
  | drainPendingRetire hDetached hInit hPending hRuntimeStep =>
      rename_i detached token runtimeId
      have hRuntime' := Runtime.Step.runtimeInvariant_preserved hRuntime hRuntimeStep
      have hVisibleNoToken : ∀ topic ∈ s.byKey, topic.token ≠ token := by
        intro topic hTopic
        have hDetachedMem := mem_of_findDetached_some hDetached
        have hDisjoint := hDestruction.2.1 detached hDetachedMem topic hTopic
        intro hEq
        apply hDisjoint
        calc
          detached.topic.token = token := detached_token_of_find hDetached
          _ = topic.token := hEq.symm
      have hVisibleRoots' := visibleRootsValid_after_rollbackRetire
        hRoots hVisibleNoToken hPending hRuntimeStep
      have hProv' := provisional_roots_after_pending_retire hFull hKeys hIds hProv
        hDestruction.2.1 hDetached hInit hPending hRuntimeStep
      have hBacked' := initializers_backed_after_rollback_retire hBacked hRuntimeStep
      refine ⟨
        hRuntime',
        hKeys,
        hIds,
        hBacked',
        hVisibleKeys,
        hVisibleTokens,
        hRtdKeys,
        hReverseRtdKeys,
        hReverseSound,
        hReverseComplete,
        hVisibleRoots',
        hProv',
        ⟨hExcelSound, hExcelComplete, hExcelOwners, hExcelBindings, hExcelCommit⟩,
        hGeneration,
        ⟨
          detached_tokens_unique_after_remove hDestruction.1,
          detached_disjoint_after_remove hDestruction.2.1,
          detached_roots_after_pending_retire hDestruction.2.2.1 hDetached
            hPending hRuntimeStep,
          detached_provisional_after_pending_retire hDestruction.2.2.2 hDetached
            hPending hRuntimeStep⟩⟩
  | drainPublishedReuse hDetached hPublished hNoPending hRegistry =>
      rename_i registry' detached token nextGeneration
      have hRuntime' := runtime_invariant_after_published_reuse hRuntime
        hNoPending hRegistry
      have hVisibleNoToken : ∀ topic ∈ s.byKey, topic.token ≠ token := by
        intro topic hTopic
        have hDetachedMem := mem_of_findDetached_some hDetached
        have hDisjoint := hDestruction.2.1 detached hDetachedMem topic hTopic
        intro hEq
        apply hDisjoint
        calc
          detached.topic.token = token := detached_token_of_find hDetached
          _ = topic.token := hEq.symm
      have hVisibleRoots' := visible_roots_after_published_reuse
        hRoots hVisibleNoToken hRegistry
      have hProv' : ({ s with
          runtime := { s.runtime with registry := registry' }
          detached := s.removeDetached token }).ProvisionalTopicsHavePendingRoots := by
        intro topic hTopic hStage
        rcases hProv topic hTopic hStage with
          ⟨init, hInitMem, hInitKey, hInitPending⟩
        exact ⟨init, hInitMem, hInitKey,
          by simpa [Runtime.State.findInitializer?] using hInitPending⟩
      refine ⟨
        hRuntime',
        hKeys,
        hIds,
        hBacked,
        hVisibleKeys,
        hVisibleTokens,
        hRtdKeys,
        hReverseRtdKeys,
        hReverseSound,
        hReverseComplete,
        hVisibleRoots',
        hProv',
        ⟨hExcelSound, hExcelComplete, hExcelOwners, hExcelBindings, hExcelCommit⟩,
        hGeneration,
        ⟨
          detached_tokens_unique_after_remove hDestruction.1,
          detached_disjoint_after_remove hDestruction.2.1,
          detached_roots_after_published_reuse (detached := detached)
            hDestruction.2.2.1 hRegistry,
          by
            intro old hOld hStage
            have hOldMem := mem_of_mem_filter_topics hOld
            rcases hDestruction.2.2.2 old hOldMem hStage with
              ⟨init, hInitMem, hInitKey, hInitPending⟩
            exact ⟨init, hInitMem, hInitKey,
              by simpa [Runtime.State.findInitializer?] using hInitPending⟩⟩⟩
  | drainPublishedRetire hDetached hPublished hNoPending hRegistry =>
      rename_i registry' detached token
      have hRuntime' := runtime_invariant_after_published_retire hRuntime
        hNoPending hRegistry
      have hVisibleNoToken : ∀ topic ∈ s.byKey, topic.token ≠ token := by
        intro topic hTopic
        have hDetachedMem := mem_of_findDetached_some hDetached
        have hDisjoint := hDestruction.2.1 detached hDetachedMem topic hTopic
        intro hEq
        apply hDisjoint
        calc
          detached.topic.token = token := detached_token_of_find hDetached
          _ = topic.token := hEq.symm
      have hVisibleRoots' := visible_roots_after_published_retire
        hRoots hVisibleNoToken hRegistry
      have hProv' : ({ s with
          runtime := { s.runtime with registry := registry' }
          detached := s.removeDetached token }).ProvisionalTopicsHavePendingRoots := by
        intro topic hTopic hStage
        rcases hProv topic hTopic hStage with
          ⟨init, hInitMem, hInitKey, hInitPending⟩
        exact ⟨init, hInitMem, hInitKey,
          by simpa [Runtime.State.findInitializer?] using hInitPending⟩
      refine ⟨
        hRuntime',
        hKeys,
        hIds,
        hBacked,
        hVisibleKeys,
        hVisibleTokens,
        hRtdKeys,
        hReverseRtdKeys,
        hReverseSound,
        hReverseComplete,
        hVisibleRoots',
        hProv',
        ⟨hExcelSound, hExcelComplete, hExcelOwners, hExcelBindings, hExcelCommit⟩,
        hGeneration,
        ⟨
          detached_tokens_unique_after_remove hDestruction.1,
          detached_disjoint_after_remove hDestruction.2.1,
          detached_roots_after_published_retire (detached := detached)
            hDestruction.2.2.1 hRegistry,
          by
            intro old hOld hStage
            have hOldMem := mem_of_mem_filter_topics hOld
            rcases hDestruction.2.2.2 old hOldMem hStage with
              ⟨init, hInitMem, hInitKey, hInitPending⟩
            exact ⟨init, hInitMem, hInitKey,
              by simpa [Runtime.State.findInitializer?] using hInitPending⟩⟩⟩
end XlFnFormal.Handle.Topics
