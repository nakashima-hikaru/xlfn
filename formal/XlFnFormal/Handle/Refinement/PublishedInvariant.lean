import XlFnFormal.Handle.Refinement.PublishedTransition
import XlFnFormal.Handle.Topics.DestructionSafety

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Refinement

open XlFnFormal.Handle.Topics

theorem topics_invariant_of_invariant
    {s : State} (hInv : s.Invariant) :
    s.topics.Invariant := by
  exact hInv.1

theorem publication_identities_unique_of_invariant
    {s : State} (hInv : s.Invariant) :
    s.PublicationIdentitiesUnique := by
  exact hInv.2.1

theorem snapshot_keys_unique_of_invariant
    {s : State} (hInv : s.Invariant) :
    s.SnapshotKeysUnique := by
  exact hInv.2.2.1

theorem warm_readers_unique_of_invariant
    {s : State} (hInv : s.Invariant) :
    s.WarmReadersUnique := by
  exact hInv.2.2.2.1

theorem live_publication_sound_of_invariant
    {s : State} (hInv : s.Invariant) :
    s.LivePublicationSound := by
  exact hInv.2.2.2.2.1

theorem provisional_publication_sound_of_invariant
    {s : State} (hInv : s.Invariant) :
    s.ProvisionalPublicationSound := by
  exact hInv.2.2.2.2.2.1

theorem live_snapshot_sound_of_invariant
    {s : State} (hInv : s.Invariant) :
    s.LiveSnapshotSound := by
  exact hInv.2.2.2.2.2.2.1

theorem live_snapshot_root_is_live_of_invariant
    {s : State} (hInv : s.Invariant) :
    s.LiveSnapshotRootIsLive := by
  exact hInv.2.2.2.2.2.2.2.1

theorem warm_reader_references_known_publication_of_invariant
    {s : State} (hInv : s.Invariant) :
    s.WarmReaderReferencesKnownPublication := by
  exact hInv.2.2.2.2.2.2.2.2.1

theorem warm_reads_bound_of_invariant
    {s : State} (hInv : s.Invariant) :
    s.WarmReadsBound := by
  exact hInv.2.2.2.2.2.2.2.2.2

theorem prepare_accounting_of_invariant
    {s : State} (hInv : s.Invariant) :
    s.PrepareAccounting := by
  exact hInv.2.2.2.2.2.2.2.2.2

private theorem no_publication_identity
    {s : State} {key : TopicKey} {token : Registry.Token}
    {publication : Publication}
    (hNone : s.findPublication? key token = none)
    (hMem : publication ∈ s.publications) :
    publication.key ≠ key ∨ publication.token ≠ token := by
  by_cases hKey : publication.key = key
  · by_cases hToken : publication.token = token
    · dsimp [State.findPublication?] at hNone
      have hSome :
          (s.publications.find? (fun candidate =>
            candidate.key == key && candidate.token == token)).isSome = true := by
        rw [List.find?_isSome]
        exact ⟨publication, hMem, by simp [hKey, hToken]⟩
      rw [hNone] at hSome
      contradiction
    · exact Or.inr hToken
  · exact Or.inl hKey

private theorem no_snapshot_key
    {s : State} {key : TopicKey} {binding : SnapshotBinding}
    (hNone : s.findSnapshot? key = none)
    (hMem : binding ∈ s.snapshot) :
    binding.key ≠ key := by
  intro hEq
  dsimp [State.findSnapshot?] at hNone
  have hSome :
      (s.snapshot.find? (fun candidate => candidate.key == key)).isSome = true := by
    rw [List.find?_isSome]
    exact ⟨binding, hMem, beq_iff_eq.mpr hEq⟩
  rw [hNone] at hSome
  contradiction

private theorem publication_key_of_find
    {s : State} {key : TopicKey} {token : Registry.Token}
    {publication : Publication}
    (hFind : s.findPublication? key token = some publication) :
    publication.key = key := by
  dsimp [State.findPublication?] at hFind
  have hPred :
      (publication.key == key) = true ∧ (publication.token == token) = true := by
    simpa only [Bool.and_eq_true] using List.find?_some hFind
  exact beq_iff_eq.mp hPred.1

private theorem publication_token_of_find
    {s : State} {key : TopicKey} {token : Registry.Token}
    {publication : Publication}
    (hFind : s.findPublication? key token = some publication) :
    publication.token = token := by
  dsimp [State.findPublication?] at hFind
  have hPred :
      (publication.key == key) = true ∧ (publication.token == token) = true := by
    simpa only [Bool.and_eq_true] using List.find?_some hFind
  exact beq_iff_eq.mp hPred.2

private theorem mem_publication_of_find
    {s : State} {key : TopicKey} {token : Registry.Token}
    {publication : Publication}
    (hFind : s.findPublication? key token = some publication) :
    publication ∈ s.publications := by
  dsimp [State.findPublication?] at hFind
  exact Runtime.List.mem_of_find?_eq_some' hFind

private theorem no_reader_id
    {s : State} {readerId : Nat} {read : WarmRead}
    (hNone : s.findWarmRead? readerId = none)
    (hMem : read ∈ s.warmReads) :
    read.id ≠ readerId := by
  intro hEq
  dsimp [State.findWarmRead?] at hNone
  have hSome :
      (s.warmReads.find? (fun candidate => candidate.id == readerId)).isSome = true := by
    rw [List.find?_isSome]
    exact ⟨read, hMem, beq_iff_eq.mpr hEq⟩
  rw [hNone] at hSome
  contradiction

private theorem filter_length_le
    {α : Type} {p : α → Bool} {items : List α} :
    (items.filter p).length ≤ items.length := by
  induction items with
  | nil => exact Nat.le_refl 0
  | cons head tail ih =>
      dsimp [List.filter]
      split
      · exact Nat.succ_le_succ ih
      · exact Nat.le_succ_of_le ih

private theorem warm_known_after_publication_update
    {s : State} {key : TopicKey} {token : Registry.Token}
    {state : PublicationState}
    (hKnown : s.WarmReaderReferencesKnownPublication) :
    ∀ read ∈ s.warmReads,
      ∃ publication ∈ s.updatePublicationState key token state,
        publication.key = read.key ∧ publication.token = read.token ∧
        publication.rtdKey = read.rtdKey := by
  intro read hRead
  rcases hKnown read hRead with ⟨old, hOld, hKey, hToken, hRtd⟩
  let mapped : Publication :=
    if old.key == key && old.token == token then { old with state := state } else old
  have hMappedMem : mapped ∈ s.updatePublicationState key token state := by
    apply List.mem_map.mpr
    exact ⟨old, hOld, rfl⟩
  refine ⟨mapped, hMappedMem, ?_, ?_, ?_⟩
  · by_cases h : old.key == key && old.token == token <;>
      simpa [mapped, h] using hKey
  · by_cases h : old.key == key && old.token == token <;>
      simpa [mapped, h] using hToken
  · by_cases h : old.key == key && old.token == token <;>
      simpa [mapped, h] using hRtd

private theorem canonical_after_disconnect_non_target
    {s : State} {topics' : Topics.State}
    {key : TopicKey} {owner : ExcelOwnerId}
    (hStep : Topics.DestructionStep s.topics
      (.disconnectTopic key owner) topics')
    {publication : Publication}
    (hCanonical : s.CanonicalTopicFor publication)
    (hKeyNe : publication.key ≠ key) :
    (State.mk topics' s.publications s.snapshot s.warmReads).CanonicalTopicFor publication := by
  rcases hCanonical with ⟨old, hOld, hOldKey, hOldToken, hOldRtd, hOldStage⟩
  have hOldKeyNe : old.key ≠ key := by
    intro hOldKeyEq
    exact hKeyNe (hOldKey.symm.trans hOldKeyEq)
  cases hStep with
  | disconnectTopic hTopic hTopicKey hTopicOwner hBinding hNoDetached =>
      refine ⟨old, ?_, hOldKey, hOldToken, hOldRtd, hOldStage⟩
      exact List.mem_filter.mpr ⟨hOld, by simp [hOldKeyNe]⟩

private theorem canonical_stage_after_disconnect_non_target
    {s : State} {topics' : Topics.State}
    {key : TopicKey} {owner : ExcelOwnerId}
    (hStep : Topics.DestructionStep s.topics
      (.disconnectTopic key owner) topics')
    {publication : Publication} {stage : Topics.TopicStage}
    (hCanonical : s.CanonicalTopicForStage publication stage)
    (hKeyNe : publication.key ≠ key) :
    ∃ topic ∈ topics'.byKey,
      topic.key = publication.key ∧ topic.token = publication.token ∧
      topic.rtdKey = publication.rtdKey ∧ topic.stage = stage := by
  rcases hCanonical with ⟨old, hOld, hOldKey, hOldToken, hOldRtd, hOldStage⟩
  have hOldKeyNe : old.key ≠ key := by
    intro hOldKeyEq
    exact hKeyNe (hOldKey.symm.trans hOldKeyEq)
  cases hStep with
  | disconnectTopic hTopic hTopicKey hTopicOwner hBinding hNoDetached =>
      refine ⟨old, List.mem_filter.mpr ⟨hOld, by simp [hOldKeyNe]⟩,
        hOldKey, hOldToken, hOldRtd, hOldStage⟩

private theorem canonical_stage_after_withdraw_non_target
    {s : State} {topics' : Topics.State}
    {key : TopicKey} {runtimeId : Runtime.InitializerId}
    (hStep : Topics.Step s.topics (.withdrawVisible key runtimeId) topics')
    {publication : Publication} {stage : Topics.TopicStage}
    (hCanonical : s.CanonicalTopicForStage publication stage)
    (hKeyNe : publication.key ≠ key) :
    ∃ topic' ∈ topics'.byKey,
      topic'.key = publication.key ∧ topic'.token = publication.token ∧
      topic'.rtdKey = publication.rtdKey ∧ topic'.stage = stage := by
  rcases hCanonical with ⟨old, hOld, hOldKey, hOldToken, hOldRtd, hOldStage⟩
  have hOldKeyNe : old.key ≠ key := by
    intro hOldKeyEq
    exact hKeyNe (hOldKey.symm.trans hOldKeyEq)
  cases hStep with
  | withdrawVisible hInit hTopic hTopicKey hExcelSettled hPending =>
      refine ⟨old, List.mem_filter.mpr ⟨hOld, by simp [hOldKeyNe]⟩,
        hOldKey, hOldToken, hOldRtd, hOldStage⟩

private theorem canonical_stage_after_detach_non_target
    {s : State} {topics' : Topics.State}
    {generation : ServerGeneration}
    (hTopics : s.topics.Invariant)
    (hStep : Topics.DestructionStep s.topics
      (.detachGeneration generation) topics')
    {publication : Publication} {stage : Topics.TopicStage}
    (hCanonical : s.CanonicalTopicForStage publication stage)
    (hNotGeneration : ∀ topic ∈ s.topics.byKey,
      topic.key = publication.key →
      topic.token = publication.token →
      topic.serverGeneration ≠ some generation) :
    ∃ topic ∈ topics'.byKey,
      topic.key = publication.key ∧ topic.token = publication.token ∧
      topic.rtdKey = publication.rtdKey ∧ topic.stage = stage := by
  rcases hCanonical with ⟨old, hOld, hOldKey, hOldToken, hOldRtd, hOldStage⟩
  have hOldGeneration := hNotGeneration old hOld hOldKey hOldToken
  have hRoots := termination_preserves_other_generation
    (hRoots := hTopics.2.2.2.2.2.2.2.2.2.2.1)
    hStep old hOld hOldGeneration
  exact ⟨old, hRoots.1, hOldKey, hOldToken, hOldRtd, hOldStage⟩

private theorem warm_known_after_generation_update
    {s : State} {generation : ServerGeneration}
    (hKnown : s.WarmReaderReferencesKnownPublication) :
    ∀ read ∈ s.warmReads,
      ∃ publication ∈ s.updateGenerationPublications generation,
        publication.key = read.key ∧ publication.token = read.token ∧
        publication.rtdKey = read.rtdKey := by
  intro read hRead
  rcases hKnown read hRead with ⟨old, hOld, hKey, hToken, hRtd⟩
  let mapped : Publication :=
    if s.topics.byKey.any (fun topic =>
        topic.key == old.key && topic.token == old.token &&
          topic.serverGeneration == some generation) then
      { old with state := .stale }
    else old
  have hMappedMem : mapped ∈ s.updateGenerationPublications generation := by
    apply List.mem_map.mpr
    exact ⟨old, hOld, rfl⟩
  refine ⟨mapped, hMappedMem, ?_, ?_, ?_⟩
  · by_cases h : s.topics.byKey.any (fun topic =>
        topic.key == old.key && topic.token == old.token &&
          topic.serverGeneration == some generation) <;>
      simpa [mapped, h] using hKey
  · by_cases h : s.topics.byKey.any (fun topic =>
        topic.key == old.key && topic.token == old.token &&
          topic.serverGeneration == some generation) <;>
      simpa [mapped, h] using hToken
  · by_cases h : s.topics.byKey.any (fun topic =>
        topic.key == old.key && topic.token == old.token &&
          topic.serverGeneration == some generation) <;>
      simpa [mapped, h] using hRtd

private theorem warm_known_after_closing_update
    {s : State} (hKnown : s.WarmReaderReferencesKnownPublication) :
    ∀ read ∈ s.warmReads,
      ∃ publication ∈ s.updateClosingPublications,
        publication.key = read.key ∧ publication.token = read.token ∧
        publication.rtdKey = read.rtdKey := by
  intro read hRead
  rcases hKnown read hRead with ⟨old, hOld, hKey, hToken, hRtd⟩
  let mapped : Publication := { old with state := closingState old.state }
  have hMem : mapped ∈ s.updateClosingPublications := by
    apply List.mem_map.mpr
    exact ⟨old, hOld, rfl⟩
  exact ⟨mapped, hMem, by simpa [mapped] using hKey,
    by simpa [mapped] using hToken, by simpa [mapped] using hRtd⟩

private theorem publication_pairwise_map_identity
    {s : State} {f : Publication → Publication}
    (hInv : s.PublicationIdentitiesUnique)
    (hKey : ∀ publication, (f publication).key = publication.key)
    (hToken : ∀ publication, (f publication).token = publication.token) :
    (s.publications.map f).Pairwise
      (fun lhs rhs => lhs.key ≠ rhs.key ∨ lhs.token ≠ rhs.token) := by
  dsimp [State.PublicationIdentitiesUnique] at hInv ⊢
  apply Topics.pairwise_map_topics hInv
  intro lhs hLhs rhs hRhs hRel
  simpa only [hKey lhs, hKey rhs, hToken lhs, hToken rhs] using hRel

private theorem snapshot_pairwise_filter
    {s : State} (hInv : s.SnapshotKeysUnique) (p : SnapshotBinding → Bool) :
    (s.snapshot.filter p).Pairwise (fun lhs rhs => lhs.key ≠ rhs.key) := by
  exact Topics.pairwise_filter_topics p hInv

private theorem warm_pairwise_filter
    {s : State} (hInv : s.WarmReadersUnique) (p : WarmRead → Bool) :
    (s.warmReads.filter p).Pairwise (fun lhs rhs => lhs.id ≠ rhs.id) := by
  exact Topics.pairwise_filter_topics p hInv

private theorem publication_pairwise_append
    {s : State} {publication : Publication}
    (hInv : s.PublicationIdentitiesUnique)
    (hSep : ∀ old ∈ s.publications,
      old.key ≠ publication.key ∨ old.token ≠ publication.token) :
    (s.publications ++ [publication]).Pairwise
      (fun lhs rhs => lhs.key ≠ rhs.key ∨ lhs.token ≠ rhs.token) := by
  dsimp [State.PublicationIdentitiesUnique] at hInv ⊢
  exact Topics.pairwise_append_singleton_topics hInv hSep

private theorem snapshot_pairwise_append
    {s : State} {binding : SnapshotBinding}
    (hInv : s.SnapshotKeysUnique)
    (hSep : ∀ old ∈ s.snapshot, old.key ≠ binding.key) :
    (s.snapshot ++ [binding]).Pairwise
      (fun lhs rhs => lhs.key ≠ rhs.key) := by
  dsimp [State.SnapshotKeysUnique] at hInv ⊢
  exact Topics.pairwise_append_singleton_topics hInv hSep

private theorem warm_pairwise_append
    {s : State} {read : WarmRead}
    (hInv : s.WarmReadersUnique)
    (hSep : ∀ old ∈ s.warmReads, old.id ≠ read.id) :
    (s.warmReads ++ [read]).Pairwise
      (fun lhs rhs => lhs.id ≠ rhs.id) := by
  dsimp [State.WarmReadersUnique] at hInv ⊢
  exact Topics.pairwise_append_singleton_topics hInv hSep

private theorem mem_update_publication_identity
    {s : State} {key : TopicKey} {token : Registry.Token}
    {state : PublicationState} {publication : Publication}
    (hMem : publication ∈ s.updatePublicationState key token state) :
    ∃ old ∈ s.publications,
      publication.key = old.key ∧
      publication.token = old.token ∧
      publication.rtdKey = old.rtdKey ∧
      publication.state =
        (if old.key == key && old.token == token then state else old.state) := by
  rcases List.mem_map.mp hMem with ⟨old, hOld, rfl⟩
  refine ⟨old, hOld, ?_, ?_, ?_, ?_⟩
  · by_cases hTarget : old.key == key && old.token == token <;>
      simp [hTarget]
  · by_cases hTarget : old.key == key && old.token == token <;>
      simp [hTarget]
  · by_cases hTarget : old.key == key && old.token == token <;>
      simp [hTarget]
  · by_cases hTarget : old.key == key && old.token == token <;>
      simp [hTarget]

private theorem target_publication_mem_update
    {s : State} {key : TopicKey} {token : Registry.Token}
    {state : PublicationState} {publication : Publication}
    (hMem : publication ∈ s.publications)
    (hKey : publication.key = key)
    (hToken : publication.token = token) :
    { publication with state := state } ∈ s.updatePublicationState key token state := by
  apply List.mem_map.mpr
  refine ⟨publication, hMem, ?_⟩
  simp [hKey, hToken]

private theorem mem_update_generation_publication_identity
    {s : State} {generation : ServerGeneration} {publication : Publication}
    (hMem : publication ∈ s.updateGenerationPublications generation) :
    ∃ old ∈ s.publications,
      publication.key = old.key ∧
      publication.token = old.token ∧
      publication.rtdKey = old.rtdKey ∧
      publication.state =
        (if s.topics.byKey.any (fun topic =>
          topic.key == old.key && topic.token == old.token &&
            topic.serverGeneration == some generation) then .stale else old.state) := by
  rcases List.mem_map.mp hMem with ⟨old, hOld, rfl⟩
  refine ⟨old, hOld, ?_, ?_, ?_, ?_⟩
  · by_cases hTarget : s.topics.byKey.any (fun topic =>
        topic.key == old.key && topic.token == old.token &&
          topic.serverGeneration == some generation) <;>
      simp [hTarget]
  · by_cases hTarget : s.topics.byKey.any (fun topic =>
        topic.key == old.key && topic.token == old.token &&
          topic.serverGeneration == some generation) <;>
      simp [hTarget]
  · by_cases hTarget : s.topics.byKey.any (fun topic =>
        topic.key == old.key && topic.token == old.token &&
          topic.serverGeneration == some generation) <;>
      simp [hTarget]
  · by_cases hTarget : s.topics.byKey.any (fun topic =>
        topic.key == old.key && topic.token == old.token &&
          topic.serverGeneration == some generation) <;>
      simp [hTarget]

private theorem mem_update_closing_publication_identity
    {s : State} {publication : Publication}
    (hMem : publication ∈ s.updateClosingPublications) :
    ∃ old ∈ s.publications,
      publication.key = old.key ∧
      publication.token = old.token ∧
      publication.rtdKey = old.rtdKey ∧
      publication.state = closingState old.state := by
  rcases List.mem_map.mp hMem with ⟨old, hOld, rfl⟩
  exact ⟨old, hOld, rfl, rfl, rfl, rfl⟩

private theorem canonical_stage_after_topic_step
    {s : State} {topics' : Topics.State} {event : Topics.Event}
    (hLiftable : topicLiftable? event = true)
    (hStep : Topics.Step s.topics event topics')
    {publication : Publication} {stage : Topics.TopicStage}
    (hCanonical : s.CanonicalTopicForStage publication stage) :
    ∃ topic ∈ topics'.byKey,
      topic.key = publication.key ∧
      topic.token = publication.token ∧
      topic.rtdKey = publication.rtdKey ∧
      topic.stage = stage := by
  rcases hCanonical with ⟨old, hOld, hKey, hToken, hRtd, hStage⟩
  cases hStep with
  | beginPrepare hRuntime => exact ⟨old, hOld, hKey, hToken, hRtd, hStage⟩
  | endPrepare hRuntime => exact ⟨old, hOld, hKey, hToken, hRtd, hStage⟩
  | beginLookup hRuntime => exact ⟨old, hOld, hKey, hToken, hRtd, hStage⟩
  | endLookup hRuntime => exact ⟨old, hOld, hKey, hToken, hRtd, hStage⟩
  | beginInitializer hNoTopic hNoInitializer hNoRuntimeId hRuntime =>
      exact ⟨old, hOld, hKey, hToken, hRtd, hStage⟩
  | insertPendingFresh hInit hNoTopic hRuntime =>
      exact ⟨old, hOld, hKey, hToken, hRtd, hStage⟩
  | insertPendingReuse hInit hNoTopic hRuntime =>
      exact ⟨old, hOld, hKey, hToken, hRtd, hStage⟩
  | publishVisible hPhase hInit hNoTopic hNoRtdKey hNoToken hNoDetachedToken hPending hRoot =>
      exact ⟨old, List.mem_append_left _ hOld, hKey, hToken, hRtd, hStage⟩
  | claimServer hTopic hTopicKey hAllowed =>
      rename_i targetKey generation
      refine ⟨_, List.mem_map.mpr ⟨old, hOld, rfl⟩, ?_, ?_, ?_, ?_⟩
      · by_cases hOldKey : old.key == targetKey <;>
          simpa [Topics.State.updateTopicServerGeneration, hOldKey] using hKey
      · by_cases hOldKey : old.key == targetKey <;>
          simpa [Topics.State.updateTopicServerGeneration, hOldKey] using hToken
      · by_cases hOldKey : old.key == targetKey <;>
          simpa [Topics.State.updateTopicServerGeneration, hOldKey] using hRtd
      · by_cases hOldKey : old.key == targetKey <;>
          simpa [Topics.State.updateTopicServerGeneration, hOldKey] using hStage
  | beginConnection hTopic hTopicKey hGeneration hTopicFree hOwnerFree =>
      rename_i targetKey owner
      refine ⟨_, List.mem_map.mpr ⟨old, hOld, rfl⟩, ?_, ?_, ?_, ?_⟩
      · by_cases hOldKey : old.key == targetKey <;>
          simpa [Topics.State.updateTopicExcel, hOldKey] using hKey
      · by_cases hOldKey : old.key == targetKey <;>
          simpa [Topics.State.updateTopicExcel, hOldKey] using hToken
      · by_cases hOldKey : old.key == targetKey <;>
          simpa [Topics.State.updateTopicExcel, hOldKey] using hRtd
      · by_cases hOldKey : old.key == targetKey <;>
          simpa [Topics.State.updateTopicExcel, hOldKey] using hStage
  | reuseCommittedConnection hTopic hTopicKey hGeneration hTopicOwner hCommitted hBinding =>
      exact ⟨old, hOld, hKey, hToken, hRtd, hStage⟩
  | commitConnection hTopic hTopicKey hGeneration hTopicOwner hNotCommitted hBinding =>
      rename_i targetKey owner
      refine ⟨_, List.mem_map.mpr ⟨old, hOld, rfl⟩, ?_, ?_, ?_, ?_⟩
      · by_cases hOldKey : old.key == targetKey <;>
          simpa [Topics.State.updateTopicExcel, hOldKey] using hKey
      · by_cases hOldKey : old.key == targetKey <;>
          simpa [Topics.State.updateTopicExcel, hOldKey] using hToken
      · by_cases hOldKey : old.key == targetKey <;>
          simpa [Topics.State.updateTopicExcel, hOldKey] using hRtd
      · by_cases hOldKey : old.key == targetKey <;>
          simpa [Topics.State.updateTopicExcel, hOldKey] using hStage
  | rollbackConnection hTopic hTopicKey hGeneration hTopicOwner hNotCommitted hBinding =>
      rename_i targetKey owner
      refine ⟨_, List.mem_map.mpr ⟨old, hOld, rfl⟩, ?_, ?_, ?_, ?_⟩
      · by_cases hOldKey : old.key == targetKey <;>
          simpa [Topics.State.updateTopicExcel, hOldKey] using hKey
      · by_cases hOldKey : old.key == targetKey <;>
          simpa [Topics.State.updateTopicExcel, hOldKey] using hToken
      · by_cases hOldKey : old.key == targetKey <;>
          simpa [Topics.State.updateTopicExcel, hOldKey] using hRtd
      · by_cases hOldKey : old.key == targetKey <;>
          simpa [Topics.State.updateTopicExcel, hOldKey] using hStage
  | commitPublication hInit hTopic hTopicKey hExcelSettled hPending hRuntime =>
      simp [topicLiftable?] at hLiftable
  | withdrawVisible hInit hTopic hTopicKey hExcelSettled hPending =>
      simp [topicLiftable?] at hLiftable
  | rollbackPendingReuse hInit hNoTopic hNoToken hNoDetached hPending hRuntime =>
      exact ⟨old, hOld, hKey, hToken, hRtd, hStage⟩
  | rollbackPendingRetire hInit hNoTopic hNoToken hNoDetached hPending hRuntime =>
      exact ⟨old, hOld, hKey, hToken, hRtd, hStage⟩
  | finishInitializer hInit hReady hRuntime =>
      exact ⟨old, hOld, hKey, hToken, hRtd, hStage⟩
  | sealTopics hRuntime =>
      simp [topicLiftable?] at hLiftable
  | closeRegistry hNoVisible hNoReverse hNoExcelOwners hNoInitializers hNoDetached hRuntime =>
      simp [topicLiftable?] at hLiftable
  | finishClose hRuntime => exact ⟨old, hOld, hKey, hToken, hRtd, hStage⟩

private theorem canonical_stage_after_publish
    {s : State} {topics' : Topics.State}
    {key : TopicKey} {runtimeId : Runtime.InitializerId} {rtdKey : RtdKey}
    (hStep : Topics.Step s.topics (.publishVisible key runtimeId rtdKey) topics')
    {publication : Publication} {stage : Topics.TopicStage}
    (hCanonical : s.CanonicalTopicForStage publication stage) :
    ∃ topic ∈ topics'.byKey,
      topic.key = publication.key ∧
      topic.token = publication.token ∧
      topic.rtdKey = publication.rtdKey ∧
      topic.stage = stage := by
  rcases hCanonical with ⟨old, hOld, hKey, hToken, hRtd, hStage⟩
  cases hStep with
  | publishVisible hPhase hInit hNoTopic hNoRtdKey hNoToken hNoDetachedToken hPending hRoot =>
      exact ⟨old, List.mem_append_left _ hOld, hKey, hToken, hRtd, hStage⟩

private theorem live_snapshot_root_of_sound
    {s : State}
    (hTopics : s.topics.Invariant)
    (hSound : s.LiveSnapshotSound) :
    s.LiveSnapshotRootIsLive := by
  intro binding hBinding
  rcases hSound binding hBinding with
    ⟨publication, hPublication, hLive, hKey, hToken, hCanonical⟩
  rcases hCanonical with ⟨topic, hTopic, hTopicKey, hTopicToken, hRtdKey, hStage⟩
  refine ⟨publication, hPublication, topic, hTopic, hLive, hKey, hToken,
    hTopicKey, hTopicToken, hRtdKey, hStage, ?_⟩
  exact hTopics.2.2.2.2.2.2.2.2.2.2.1 topic hTopic

private theorem canonical_stage_after_commit
    {s : State} {topics' : Topics.State}
    {key : TopicKey} {runtimeId : Runtime.InitializerId}
    (hStep : Topics.Step s.topics (.commitPublication key runtimeId) topics')
    {publication : Publication} {topic : Topic}
    (hCanonical : s.CanonicalTopicForStage publication .committed)
    (hKeyNe : publication.key ≠ key) :
    ∃ topic' ∈ topics'.byKey,
      topic'.key = publication.key ∧
      topic'.token = publication.token ∧
      topic'.rtdKey = publication.rtdKey ∧
      topic'.stage = .committed := by
  rcases hCanonical with ⟨old, hOld, hOldKey, hOldToken, hOldRtd, hOldStage⟩
  cases hStep with
  | commitPublication hInit hTopic hTopicKey hExcelSettled hPending hRuntime =>
      refine ⟨if old.key == key then { old with stage := .committed } else old,
        List.mem_map.mpr ⟨old, hOld, rfl⟩, ?_, ?_, ?_, ?_⟩
      · by_cases hOldKeyBool : old.key == key <;>
          simpa [Topics.State.updateTopicStage, hOldKeyBool] using hOldKey
      · by_cases hOldKeyBool : old.key == key <;>
          simpa [Topics.State.updateTopicStage, hOldKeyBool] using hOldToken
      · by_cases hOldKeyBool : old.key == key <;>
          simpa [Topics.State.updateTopicStage, hOldKeyBool] using hOldRtd
      · by_cases hOldKeyBool : old.key == key
        · have hOldKeyEq : old.key = key := beq_iff_eq.mp hOldKeyBool
          exact False.elim (hKeyNe (hOldKey.symm.trans hOldKeyEq))
        · simp [hOldKeyBool]
          exact hOldStage

private theorem live_publications_after_topic_step
    {s : State} {topics' : Topics.State} {event : Topics.Event}
    (hLiftable : topicLiftable? event = true)
    (hStep : Topics.Step s.topics event topics')
    (hSound : s.LivePublicationSound) :
    ∀ publication ∈ s.publications,
      publication.state = .live →
        (State.mk topics' s.publications s.snapshot s.warmReads).CanonicalTopicFor publication := by
  intro publication hPublication hLive
  have hCanonical := hSound publication hPublication hLive
  exact canonical_stage_after_topic_step hLiftable hStep hCanonical

private theorem provisional_publications_after_topic_step
    {s : State} {topics' : Topics.State} {event : Topics.Event}
    (hLiftable : topicLiftable? event = true)
    (hStep : Topics.Step s.topics event topics')
    (hSound : s.ProvisionalPublicationSound) :
    ∀ publication ∈ s.publications,
      publication.state = .provisional →
        (State.mk topics' s.publications s.snapshot s.warmReads).CanonicalTopicForStage
          publication .provisional := by
  intro publication hPublication hProvisional
  have hCanonical := hSound publication hPublication hProvisional
  exact canonical_stage_after_topic_step hLiftable hStep hCanonical

private theorem live_snapshot_sound_after_topic_step
    {s : State} {topics' : Topics.State} {event : Topics.Event}
    (hLiftable : topicLiftable? event = true)
    (hStep : Topics.Step s.topics event topics')
    (hSound : s.LiveSnapshotSound) :
    ∀ binding ∈ s.snapshot,
      ∃ publication ∈ s.publications,
        publication.state = .live ∧
        publication.key = binding.key ∧
        publication.token = binding.token ∧
        (State.mk topics' s.publications s.snapshot s.warmReads).CanonicalTopicFor publication := by
  intro binding hBinding
  rcases hSound binding hBinding with
    ⟨publication, hPublication, hLive, hKey, hToken, hCanonical⟩
  refine ⟨publication, hPublication, hLive, hKey, hToken, ?_⟩
  exact canonical_stage_after_topic_step hLiftable hStep hCanonical

private theorem committed_target_after_commit
    {s : State} {topics' : Topics.State}
    {key : TopicKey} {runtimeId : Runtime.InitializerId}
    {topic : Topics.Topic}
    (hTopic : s.topics.findTopic? key = some topic)
    (hTopicKey : topic.key = key)
    (hStep : Topics.Step s.topics (.commitPublication key runtimeId) topics') :
    ∃ target ∈ topics'.byKey,
      target.key = key ∧ target.token = topic.token ∧
      target.rtdKey = topic.rtdKey ∧ target.stage = .committed := by
  cases hStep with
  | commitPublication hInit hTopic' hTopicKey' hExcelSettled hPending hRuntime =>
      rename_i source
      have hEq : { source with stage := .provisional } = topic :=
        Option.some.inj (hTopic'.symm.trans hTopic)
      cases hEq
      refine ⟨{ ({ source with stage := .provisional }) with stage := .committed },
        ?_, ?_, ?_, ?_, ?_⟩
      · apply List.mem_map.mpr
        refine ⟨{ source with stage := .provisional }, ?_, ?_⟩
        · exact Topics.mem_of_findTopic_some hTopic'
        · simp [hTopicKey']
      · exact hTopicKey'
      · rfl
      · rfl
      · rfl

private theorem canonical_key_ne_provisional_topic
    {s : State} {key : TopicKey} {topic : Topics.Topic}
    (hTopics : s.topics.Invariant)
    (hTopic : s.topics.findTopic? key = some topic)
    (hTopicKey : topic.key = key)
    {publication : Publication} {stage : Topics.TopicStage}
    (hCanonical : s.CanonicalTopicForStage publication stage)
    (hDifferent : publication.key ≠ key ∨ publication.token ≠ topic.token) :
    publication.key ≠ key := by
  intro hPublicationKey
  rcases hCanonical with ⟨old, hOld, hOldKey, hOldToken, hOldRtd, hOldStage⟩
  have hTopicMem := Topics.mem_of_findTopic_some hTopic
  have hOldKey' : old.key = key := hOldKey.trans hPublicationKey
  have hSame : old = topic := Topics.topic_eq_of_same_key
    hTopics.2.2.2.2.1 hOld hTopicMem hOldKey' hTopicKey
  have hTokenEq : publication.token = topic.token := by
    exact hOldToken.symm.trans (by simp [hSame])
  rcases hDifferent with hKeyNe | hTokenNe
  · exact hKeyNe hPublicationKey
  · exact hTokenNe hTokenEq

private theorem canonical_stage_after_commit_non_target
    {s : State} {topics' : Topics.State}
    {key : TopicKey} {runtimeId : Runtime.InitializerId}
    (hStep : Topics.Step s.topics (.commitPublication key runtimeId) topics')
    {publication : Publication} {stage : Topics.TopicStage}
    (hCanonical : s.CanonicalTopicForStage publication stage)
    (hKeyNe : publication.key ≠ key) :
    ∃ topic' ∈ topics'.byKey,
      topic'.key = publication.key ∧
      topic'.token = publication.token ∧
      topic'.rtdKey = publication.rtdKey ∧
      topic'.stage = stage := by
  rcases hCanonical with ⟨old, hOld, hOldKey, hOldToken, hOldRtd, hOldStage⟩
  have hOldKeyNe : old.key ≠ key := by
    intro hOldKeyEq
    exact hKeyNe (hOldKey.symm.trans hOldKeyEq)
  cases hStep with
  | commitPublication hInit hTopic hTopicKey hExcelSettled hPending hRuntime =>
      refine ⟨if old.key == key then { old with stage := .committed } else old,
        List.mem_map.mpr ⟨old, hOld, rfl⟩, ?_, ?_, ?_, ?_⟩
      · by_cases hOldKeyBool : old.key == key
        · simpa [Topics.State.updateTopicStage, hOldKeyBool] using hOldKey
        · simpa [Topics.State.updateTopicStage, hOldKeyBool] using hOldKey
      · by_cases hOldKeyBool : old.key == key
        · simpa [Topics.State.updateTopicStage, hOldKeyBool] using hOldToken
        · simpa [Topics.State.updateTopicStage, hOldKeyBool] using hOldToken
      · by_cases hOldKeyBool : old.key == key
        · simpa [Topics.State.updateTopicStage, hOldKeyBool] using hOldRtd
        · simpa [Topics.State.updateTopicStage, hOldKeyBool] using hOldRtd
      · by_cases hOldKeyBool : old.key == key
        · have hOldKeyEq : old.key = key := beq_iff_eq.mp hOldKeyBool
          exact False.elim (hOldKeyNe hOldKeyEq)
        · simpa [Topics.State.updateTopicStage, hOldKeyBool] using hOldStage

private theorem canonical_stage_after_pending_reuse
    {s : State} {topics' : Topics.State}
    {token : Registry.Token} {runtimeId : Runtime.InitializerId}
    {nextGeneration : Registry.Generation}
    (hStep : Topics.DestructionStep s.topics
      (.drainPendingReuse token runtimeId nextGeneration) topics')
    {publication : Publication} {stage : Topics.TopicStage}
    (hCanonical : s.CanonicalTopicForStage publication stage) :
    (State.mk topics' s.publications s.snapshot s.warmReads).CanonicalTopicForStage
      publication stage := by
  cases hStep
  exact hCanonical

private theorem canonical_stage_after_pending_retire
    {s : State} {topics' : Topics.State}
    {token : Registry.Token} {runtimeId : Runtime.InitializerId}
    (hStep : Topics.DestructionStep s.topics
      (.drainPendingRetire token runtimeId) topics')
    {publication : Publication} {stage : Topics.TopicStage}
    (hCanonical : s.CanonicalTopicForStage publication stage) :
    (State.mk topics' s.publications s.snapshot s.warmReads).CanonicalTopicForStage
      publication stage := by
  cases hStep
  exact hCanonical

private theorem canonical_stage_after_published_reuse
    {s : State} {topics' : Topics.State}
    {token : Registry.Token} {nextGeneration : Registry.Generation}
    (hStep : Topics.DestructionStep s.topics
      (.drainPublishedReuse token nextGeneration) topics')
    {publication : Publication} {stage : Topics.TopicStage}
    (hCanonical : s.CanonicalTopicForStage publication stage) :
    (State.mk topics' s.publications s.snapshot s.warmReads).CanonicalTopicForStage
      publication stage := by
  cases hStep
  exact hCanonical

private theorem canonical_stage_after_published_retire
    {s : State} {topics' : Topics.State}
    {token : Registry.Token}
    (hStep : Topics.DestructionStep s.topics
      (.drainPublishedRetire token) topics')
    {publication : Publication} {stage : Topics.TopicStage}
    (hCanonical : s.CanonicalTopicForStage publication stage) :
    (State.mk topics' s.publications s.snapshot s.warmReads).CanonicalTopicForStage
      publication stage := by
  cases hStep
  exact hCanonical

theorem Step.invariant_preserved
    {s s' : State} {event : Event}
    (hInv : s.Invariant)
    (hStep : Step s event s') :
    s'.Invariant := by
  rcases hInv with
    ⟨hTopics, hPublications, hSnapshots, hWarmReaders, hLivePublications,
      hProvisionalPublications, hLiveSnapshots, hSnapshotRoots, hWarmKnown,
      hBound⟩
  cases hStep with
  | liftTopic hLiftable hTopicStep hBound' =>
      have hTopics' := Topics.Step.invariant_preserved hTopics hTopicStep
      have hLive' := live_publications_after_topic_step
        hLiftable hTopicStep hLivePublications
      have hProvisional' := provisional_publications_after_topic_step
        hLiftable hTopicStep hProvisionalPublications
      have hSnapshots' := live_snapshot_sound_after_topic_step
        hLiftable hTopicStep hLiveSnapshots
      have hSnapshotRoots' := live_snapshot_root_of_sound
        (s := { s with topics := _ }) hTopics' hSnapshots'
      refine ⟨hTopics', hPublications, hSnapshots, hWarmReaders, ?_, ?_,
        ?_, ?_, ?_, hBound'⟩
      · exact hLive'
      · exact hProvisional'
      · exact hSnapshots'
      · exact hSnapshotRoots'
      · exact hWarmKnown
  | publishAndInstallProvisional hTopicStep hTopic hTopicKey hTopicToken hTopicRtdKey
      hStage hNoPublication hNoSnapshot =>
      rename_i topics' topic key runtimeId token rtdKey
      have hTopics' := Topics.Step.invariant_preserved hTopics hTopicStep
      have hTopicMem := Topics.mem_of_findTopic_some hTopic
      have hSep : ∀ old ∈ s.publications,
          old.key ≠ key ∨ old.token ≠ token := by
        intro old hOld
        exact no_publication_identity (key := key) (token := token)
          hNoPublication hOld
      let newPublication : Publication :=
        { key := key, token := token, rtdKey := rtdKey, state := .provisional }
      have hPublicationUnique :
          (s.publications ++ [newPublication]).Pairwise
            (fun lhs rhs => lhs.key ≠ rhs.key ∨ lhs.token ≠ rhs.token) := by
        apply publication_pairwise_append hPublications
        intro old hOld
        simpa [newPublication] using hSep old hOld
      have hNewCanonical :
          ∃ topic ∈ topics'.byKey,
            topic.key = key ∧ topic.token = token ∧
            topic.rtdKey = rtdKey ∧ topic.stage = .provisional := by
        exact ⟨topic, hTopicMem, hTopicKey, hTopicToken, hTopicRtdKey, hStage⟩
      have hLive' : ∀ publication ∈ s.publications,
          publication.state = .live →
          (State.mk topics' (s.publications ++ [newPublication])
            s.snapshot s.warmReads).CanonicalTopicFor publication := by
        intro publication hPublication hLive
        have hCanonical := hLivePublications publication hPublication hLive
        exact canonical_stage_after_publish hTopicStep hCanonical
      have hProvisional' : ∀ publication ∈ s.publications,
          publication.state = .provisional →
          (State.mk topics' (s.publications ++ [newPublication])
            s.snapshot s.warmReads).CanonicalTopicForStage publication .provisional := by
        intro publication hPublication hProvisional
        have hCanonical := hProvisionalPublications publication hPublication hProvisional
        exact canonical_stage_after_publish hTopicStep hCanonical
      have hSnapshots' : ∀ binding ∈ s.snapshot,
          ∃ publication ∈ s.publications ++ [newPublication],
            publication.state = .live ∧
            publication.key = binding.key ∧
            publication.token = binding.token ∧
            (State.mk topics' (s.publications ++ [newPublication])
              s.snapshot s.warmReads).CanonicalTopicFor publication := by
        intro binding hBinding
        rcases hLiveSnapshots binding hBinding with
          ⟨publication, hPublication, hLive, hKey, hToken, hCanonical⟩
        exact ⟨publication, List.mem_append_left _ hPublication, hLive, hKey, hToken,
          canonical_stage_after_publish hTopicStep hCanonical⟩
      refine ⟨hTopics', hPublicationUnique, hSnapshots, hWarmReaders, ?_, ?_,
        ?_, ?_, ?_, ?_⟩
      · intro publication hMem hLive
        simp only [List.mem_append, List.mem_singleton] at hMem
        cases hMem with
        | inl hOld => exact hLive' publication hOld hLive
        | inr hNew => subst hNew; cases hLive
      · intro publication hMem hProvisional
        simp only [List.mem_append, List.mem_singleton] at hMem
        cases hMem with
        | inl hOld => exact hProvisional' publication hOld hProvisional
        | inr hNew =>
            subst hNew
            exact hNewCanonical
      · intro binding hBinding
        exact hSnapshots' binding hBinding
      · exact live_snapshot_root_of_sound
          (s := State.mk topics' (s.publications ++ [newPublication])
            s.snapshot s.warmReads) hTopics' hSnapshots'
      · intro read hRead
        rcases hWarmKnown read hRead with
          ⟨publication, hPublication, hKey, hToken, hRtdKey⟩
        exact ⟨publication, List.mem_append_left _ hPublication, hKey, hToken, hRtdKey⟩
      · cases hTopicStep
        exact hBound
  | withdrawAndInvalidate hTopic hTopicKey hTopicToken hPublication
      hPublicationRtdKey hStage hPublicationState hNoSnapshot hTopicStep =>
      rename_i topics' topic publication0 key runtimeId token
      have hTopics' := Topics.Step.invariant_preserved hTopics hTopicStep
      have hPublicationKey := publication_key_of_find hPublication
      have hPublicationToken := publication_token_of_find hPublication
      have hPublicationMem := mem_publication_of_find hPublication
      have hPublicationUnique :
          (s.updatePublicationState key token .stale).Pairwise
            (fun lhs rhs => lhs.key ≠ rhs.key ∨ lhs.token ≠ rhs.token) := by
        apply publication_pairwise_map_identity hPublications
        · intro publication
          split <;> rfl
        · intro publication
          split <;> rfl
      have hLiveSnapshots' :
          ∀ binding ∈ s.snapshot,
            ∃ publication ∈ s.updatePublicationState key token .stale,
              publication.state = .live ∧
              publication.key = binding.key ∧
              publication.token = binding.token ∧
              (State.mk topics' (s.updatePublicationState key token .stale)
                s.snapshot s.warmReads).CanonicalTopicFor publication := by
        intro binding hBinding
        rcases hLiveSnapshots binding hBinding with
          ⟨old, hOld, hOldLive, hBindingKey, hBindingToken, hOldCanonical⟩
        have hBindingKeyNe : binding.key ≠ key := no_snapshot_key hNoSnapshot hBinding
        have hOldKeyNe : old.key ≠ key := by
          intro hOldKey
          exact hBindingKeyNe (hBindingKey.symm.trans hOldKey)
        have hCanonical := canonical_stage_after_withdraw_non_target
          hTopicStep hOldCanonical hOldKeyNe
        have hMappedMem : old ∈ s.updatePublicationState key token .stale := by
          apply List.mem_map.mpr
          exact ⟨old, hOld, by simp [hOldKeyNe]⟩
        exact ⟨old, hMappedMem, hOldLive, hBindingKey, hBindingToken, hCanonical⟩
      refine ⟨hTopics', hPublicationUnique, hSnapshots, hWarmReaders, ?_, ?_,
        ?_, ?_, ?_, ?_⟩
      · intro publication' hMem hLive
        rcases mem_update_publication_identity hMem with
          ⟨old, hOld, hKey, hToken, hRtd, hState⟩
        by_cases hTarget : old.key == key && old.token == token
        · have hStale : publication'.state = .stale := by
            simpa [hTarget] using hState
          exact False.elim (by cases hLive.symm.trans hStale)
        · have hOldLive : old.state = .live := by
            have hState' : publication'.state = old.state := by
              simpa [hTarget] using hState
            exact hState'.symm.trans hLive
          have hOldCanonical := hLivePublications old hOld hOldLive
          have hDifferent : old.key ≠ key ∨ old.token ≠ token := by
            by_cases hOldKey : old.key = key
            · right
              intro hOldToken
              apply hTarget
              simp [hOldKey, hOldToken]
            · exact Or.inl hOldKey
          have hDifferent' : old.key ≠ key ∨ old.token ≠ topic.token := by
            rcases hDifferent with hKeyNe | hTokenNe
            · exact Or.inl hKeyNe
            · right
              intro hEq
              apply hTokenNe
              exact hEq.trans hTopicToken
          have hOldKeyNe := canonical_key_ne_provisional_topic
            hTopics hTopic hTopicKey hOldCanonical hDifferent'
          have hCanonical := canonical_stage_after_withdraw_non_target
            hTopicStep hOldCanonical hOldKeyNe
          rcases hCanonical with
            ⟨newTopic, hNewMem, hNewKey, hNewToken, hNewRtd, hNewStage⟩
          exact ⟨newTopic, hNewMem, hNewKey.trans hKey.symm,
            hNewToken.trans hToken.symm, hNewRtd.trans hRtd.symm, hNewStage⟩
      · intro publication' hMem hProvisional
        rcases mem_update_publication_identity hMem with
          ⟨old, hOld, hKey, hToken, hRtd, hState⟩
        by_cases hTarget : old.key == key && old.token == token
        · have hStale : publication'.state = .stale := by
            simpa [hTarget] using hState
          exact False.elim (by cases hProvisional.symm.trans hStale)
        · have hOldProvisional : old.state = .provisional := by
            have hState' : publication'.state = old.state := by
              simpa [hTarget] using hState
            exact hState'.symm.trans hProvisional
          have hOldCanonical := hProvisionalPublications old hOld hOldProvisional
          have hDifferent : old.key ≠ key ∨ old.token ≠ token := by
            by_cases hOldKey : old.key = key
            · right
              intro hOldToken
              apply hTarget
              simp [hOldKey, hOldToken]
            · exact Or.inl hOldKey
          have hDifferent' : old.key ≠ key ∨ old.token ≠ topic.token := by
            rcases hDifferent with hKeyNe | hTokenNe
            · exact Or.inl hKeyNe
            · right
              intro hEq
              apply hTokenNe
              exact hEq.trans hTopicToken
          have hOldKeyNe := canonical_key_ne_provisional_topic
            hTopics hTopic hTopicKey hOldCanonical hDifferent'
          have hCanonical := canonical_stage_after_withdraw_non_target
            hTopicStep hOldCanonical hOldKeyNe
          rcases hCanonical with
            ⟨newTopic, hNewMem, hNewKey, hNewToken, hNewRtd, hNewStage⟩
          exact ⟨newTopic, hNewMem, hNewKey.trans hKey.symm,
            hNewToken.trans hToken.symm, hNewRtd.trans hRtd.symm, hNewStage⟩
      · exact hLiveSnapshots'
      · exact live_snapshot_root_of_sound
          (s := State.mk topics' (s.updatePublicationState key token .stale)
            s.snapshot s.warmReads) hTopics' hLiveSnapshots'
      · intro read hRead
        rcases hWarmKnown read hRead with
          ⟨old, hOld, hKey, hToken, hRtd⟩
        let mapped : Publication :=
          if old.key == key && old.token == token then
            { old with state := .stale }
          else old
        have hMappedMem : mapped ∈ s.updatePublicationState key token .stale := by
          apply List.mem_map.mpr
          exact ⟨old, hOld, rfl⟩
        refine ⟨mapped, hMappedMem, ?_, ?_, ?_⟩
        · by_cases h : old.key == key && old.token == token <;>
            simpa [mapped, h] using hKey
        · by_cases h : old.key == key && old.token == token <;>
            simpa [mapped, h] using hToken
        · by_cases h : old.key == key && old.token == token <;>
            simpa [mapped, h] using hRtd
      · cases hTopicStep
        exact hBound
  | commitAndActivate hPublication hTopic hTopicKey hTopicToken hTopicRtdKey
      hStage hPublicationState hNoSnapshot hTopicStep =>
      rename_i topics' publication topic key runtimeId token
      have hTopics' := Topics.Step.invariant_preserved hTopics hTopicStep
      have hPublicationKey := publication_key_of_find hPublication
      have hPublicationToken := publication_token_of_find hPublication
      have hPublicationMem := mem_publication_of_find hPublication
      have hTargetCanonical := committed_target_after_commit
        hTopic hTopicKey hTopicStep
      have hTargetCanonical' :
          ∃ target ∈ topics'.byKey,
            target.key = key ∧ target.token = token ∧
            target.rtdKey = publication.rtdKey ∧ target.stage = .committed := by
        rcases hTargetCanonical with
          ⟨target, hTarget, hTargetKey, hTargetToken, hTargetRtd, hTargetStage⟩
        refine ⟨target, hTarget, hTargetKey, ?_, ?_, hTargetStage⟩
        simpa [hTopicToken] using hTargetToken
        simpa [hTopicRtdKey] using hTargetRtd
      have hTargetCanonicalForPublication :
          (State.mk topics' (s.updatePublicationState key token .live)
            (s.snapshot ++ [{ key := key, token := token }]) s.warmReads).CanonicalTopicFor
            { publication with state := .live } := by
        rcases hTargetCanonical' with
          ⟨target, hTarget, hTargetKey, hTargetToken, hTargetRtd, hTargetStage⟩
        refine ⟨target, hTarget, ?_, ?_, hTargetRtd, hTargetStage⟩
        · exact hTargetKey.trans hPublicationKey.symm
        · exact hTargetToken.trans hPublicationToken.symm
      have hAccounting' :
          s.warmReads.length + topics'.runtime.initializers.length
            ≤ topics'.runtime.activePrepares := by
        cases hTopicStep with
        | commitPublication hInit hTopic' hTopicKey' hExcelSettled hPending hRuntime =>
            cases hRuntime
            simpa [State.PrepareAccounting, Runtime.State.updateInitializer] using hBound
      have hPublicationUnique :
          (s.updatePublicationState key token .live).Pairwise
            (fun lhs rhs => lhs.key ≠ rhs.key ∨ lhs.token ≠ rhs.token) := by
        have hMapped := publication_pairwise_map_identity (s := s)
          (f := fun p =>
            if p.key == key && p.token == token then { p with state := .live } else p)
          hPublications
          (by intro p; by_cases h : p.key == key && p.token == token <;>
            simp [h])
          (by intro p; by_cases h : p.key == key && p.token == token <;>
            simp [h])
        simpa [State.updatePublicationState] using hMapped
      have hTargetPublicationMem :
          { publication with state := .live } ∈
            s.updatePublicationState key token .live := by
        exact target_publication_mem_update hPublicationMem
          hPublicationKey hPublicationToken
      refine ⟨hTopics', hPublicationUnique, ?_, hWarmReaders, ?_, ?_,
        ?_, ?_, ?_, ?_⟩
      · dsimp [State.SnapshotKeysUnique] at hSnapshots ⊢
        exact Topics.pairwise_append_singleton_topics hSnapshots (by
          intro binding hBinding
          exact no_snapshot_key hNoSnapshot hBinding)
      · intro publication' hMem hLive
        rcases mem_update_publication_identity hMem with
          ⟨old, hOld, hKey, hToken, hRtd, hState⟩
        by_cases hTarget : old.key == key && old.token == token
        · have hTargetParts :
              (old.key == key) = true ∧ (old.token == token) = true := by
            simpa only [Bool.and_eq_true] using hTarget
          have hOldKey : old.key = key := beq_iff_eq.mp hTargetParts.1
          have hOldToken : old.token = token := beq_iff_eq.mp hTargetParts.2
          have hOldRtd : old.rtdKey = publication.rtdKey := by
            by_cases hEq : old = publication
            · cases hEq
              rfl
            · rcases Topics.pairwise_mem_ne_topics hPublications hOld
                hPublicationMem hEq with hRel | hRel
              · rcases hRel with hRel | hRel
                · exact False.elim (hRel (hOldKey.trans hPublicationKey.symm))
                · exact False.elim (hRel (hOldToken.trans hPublicationToken.symm))
              · rcases hRel with hRel | hRel
                · exact False.elim (hRel (hPublicationKey.trans hOldKey.symm))
                · exact False.elim (hRel (hPublicationToken.trans hOldToken.symm))
          have hPublicationKey' : publication'.key = key := hKey.trans hOldKey
          have hPublicationToken' : publication'.token = token := hToken.trans hOldToken
          have hPublicationRtd' : publication'.rtdKey = publication.rtdKey :=
            hRtd.trans hOldRtd
          rcases hTargetCanonical' with
            ⟨target, hTargetMem, hTargetKey, hTargetToken, hTargetRtd, hTargetStage⟩
          exact ⟨target, hTargetMem, hTargetKey.trans hPublicationKey'.symm,
            hTargetToken.trans hPublicationToken'.symm,
            hTargetRtd.trans hPublicationRtd'.symm, hTargetStage⟩
        · have hOldLive : old.state = .live := by
            have hState' : publication'.state = old.state := by
              simpa [hTarget] using hState
            exact hState'.symm.trans hLive
          have hOldCanonical := hLivePublications old hOld hOldLive
          have hDifferent : old.key ≠ key ∨ old.token ≠ token := by
            by_cases hOldKey : old.key = key
            · right
              intro hOldToken
              apply hTarget
              simp [hOldKey, hOldToken]
            · exact Or.inl hOldKey
          have hDifferent' : old.key ≠ key ∨ old.token ≠ topic.token := by
            rcases hDifferent with hKeyNe | hTokenNe
            · exact Or.inl hKeyNe
            · right
              intro hEq
              apply hTokenNe
              exact hEq.trans hTopicToken
          have hKeyNe := canonical_key_ne_provisional_topic
            hTopics hTopic hTopicKey hOldCanonical hDifferent'
          have hCanonical := canonical_stage_after_commit_non_target
            hTopicStep hOldCanonical hKeyNe
          rcases hCanonical with ⟨newTopic, hNewMem, hNewKey, hNewToken, hNewRtd, hNewStage⟩
          exact ⟨newTopic, hNewMem, hNewKey.trans hKey.symm,
            hNewToken.trans hToken.symm, hNewRtd.trans hRtd.symm, hNewStage⟩
      · intro publication' hMem hProvisional
        rcases mem_update_publication_identity hMem with
          ⟨old, hOld, hKey, hToken, hRtd, hState⟩
        by_cases hTarget : old.key == key && old.token == token
        · have hLive : publication'.state = .live := by
            simpa [hTarget] using hState
          cases hLive.symm.trans hProvisional
        · have hOldProvisional : old.state = .provisional := by
            have hState' : publication'.state = old.state := by
              simpa [hTarget] using hState
            exact hState'.symm.trans hProvisional
          have hOldCanonical := hProvisionalPublications old hOld hOldProvisional
          have hDifferent : old.key ≠ key ∨ old.token ≠ token := by
            by_cases hOldKey : old.key = key
            · right
              intro hOldToken
              apply hTarget
              simp [hOldKey, hOldToken]
            · exact Or.inl hOldKey
          have hDifferent' : old.key ≠ key ∨ old.token ≠ topic.token := by
            rcases hDifferent with hKeyNe | hTokenNe
            · exact Or.inl hKeyNe
            · right
              intro hEq
              apply hTokenNe
              exact hEq.trans hTopicToken
          have hKeyNe := canonical_key_ne_provisional_topic
            hTopics hTopic hTopicKey hOldCanonical hDifferent'
          have hCanonical := canonical_stage_after_commit_non_target
            hTopicStep hOldCanonical hKeyNe
          rcases hCanonical with ⟨newTopic, hNewMem, hNewKey, hNewToken, hNewRtd, hNewStage⟩
          exact ⟨newTopic, hNewMem, hNewKey.trans hKey.symm,
            hNewToken.trans hToken.symm, hNewRtd.trans hRtd.symm, hNewStage⟩
      · intro binding hBinding
        simp only [List.mem_append, List.mem_singleton] at hBinding
        cases hBinding with
        | inr hNew =>
            subst hNew
            refine ⟨{ publication with state := .live }, hTargetPublicationMem,
              rfl, ?_, ?_, hTargetCanonicalForPublication⟩
            · exact hPublicationKey
            · exact hPublicationToken
        | inl hOldBinding =>
            rcases hLiveSnapshots binding hOldBinding with
              ⟨oldPublication, hOldPublication, hOldLive, hBindingKey,
                hBindingToken, hOldCanonical⟩
            have hBindingKeyNe : binding.key ≠ key := no_snapshot_key hNoSnapshot hOldBinding
            have hOldKeyNe : oldPublication.key ≠ key := by
              intro hEq
              exact hBindingKeyNe (hBindingKey.symm.trans hEq)
            have hCanonical := canonical_stage_after_commit_non_target
              hTopicStep hOldCanonical hOldKeyNe
            let mapped : Publication :=
              if oldPublication.key == key && oldPublication.token == token then
                { oldPublication with state := .live }
              else oldPublication
            have hMappedMem : mapped ∈ s.updatePublicationState key token .live := by
              apply List.mem_map.mpr
              exact ⟨oldPublication, hOldPublication, rfl⟩
            have hMappedFields :
                mapped.key = oldPublication.key ∧
                mapped.token = oldPublication.token ∧
                mapped.rtdKey = oldPublication.rtdKey := by
              by_cases h : oldPublication.key == key && oldPublication.token == token <;>
                simp [mapped, h]
            refine ⟨mapped, hMappedMem, ?_, ?_, ?_, ?_⟩
            · by_cases h : oldPublication.key == key && oldPublication.token == token <;>
                simp [mapped, h, hOldLive]
            · exact hMappedFields.1.trans hBindingKey
            · exact hMappedFields.2.1.trans hBindingToken
            · rcases hCanonical with
                ⟨newTopic, hNewMem, hNewKey, hNewToken, hNewRtd, hNewStage⟩
              exact ⟨newTopic, hNewMem, hNewKey.trans hMappedFields.1.symm,
                hNewToken.trans hMappedFields.2.1.symm,
                hNewRtd.trans hMappedFields.2.2.symm, hNewStage⟩
      · exact live_snapshot_root_of_sound hTopics' (by
          intro binding hBinding
          simp only [List.mem_append, List.mem_singleton] at hBinding
          cases hBinding with
          | inr hNew =>
              subst hNew
              refine ⟨{ publication with state := .live }, hTargetPublicationMem,
                rfl, ?_, ?_, hTargetCanonicalForPublication⟩
              · exact hPublicationKey
              · exact hPublicationToken
          | inl hOldBinding =>
              rcases hLiveSnapshots binding hOldBinding with
                ⟨oldPublication, hOldPublication, hOldLive, hBindingKey,
                  hBindingToken, hOldCanonical⟩
              have hBindingKeyNe := no_snapshot_key hNoSnapshot hOldBinding
              have hOldKeyNe : oldPublication.key ≠ key := by
                intro hEq
                exact hBindingKeyNe (hBindingKey.symm.trans hEq)
              have hCanonical := canonical_stage_after_commit_non_target
                hTopicStep hOldCanonical hOldKeyNe
              let mapped : Publication :=
                if oldPublication.key == key && oldPublication.token == token then
                  { oldPublication with state := .live }
                else oldPublication
              have hMappedMem : mapped ∈ s.updatePublicationState key token .live := by
                apply List.mem_map.mpr
                exact ⟨oldPublication, hOldPublication, rfl⟩
              have hMappedFields :
                  mapped.key = oldPublication.key ∧
                  mapped.token = oldPublication.token ∧
                  mapped.rtdKey = oldPublication.rtdKey := by
                by_cases h : oldPublication.key == key && oldPublication.token == token <;>
                  simp [mapped, h]
              refine ⟨mapped, hMappedMem, ?_, ?_, ?_, ?_⟩
              · by_cases h : oldPublication.key == key && oldPublication.token == token <;>
                  simp [mapped, h, hOldLive]
              · exact hMappedFields.1.trans hBindingKey
              · exact hMappedFields.2.1.trans hBindingToken
              · rcases hCanonical with
                  ⟨newTopic, hNewMem, hNewKey, hNewToken, hNewRtd, hNewStage⟩
                exact ⟨newTopic, hNewMem, hNewKey.trans hMappedFields.1.symm,
                  hNewToken.trans hMappedFields.2.1.symm,
                  hNewRtd.trans hMappedFields.2.2.symm, hNewStage⟩)
      · intro read hRead
        rcases hWarmKnown read hRead with
          ⟨oldPublication, hOldPublication, hKey, hToken, hRtd⟩
        let mapped : Publication :=
          if oldPublication.key == key && oldPublication.token == token then
            { oldPublication with state := .live }
          else oldPublication
        have hMappedMem : mapped ∈ s.updatePublicationState key token .live := by
          apply List.mem_map.mpr
          exact ⟨oldPublication, hOldPublication, rfl⟩
        refine ⟨mapped, hMappedMem, ?_, ?_, ?_⟩
        · by_cases h : oldPublication.key == key && oldPublication.token == token <;>
            simpa [mapped, h] using hKey
        · by_cases h : oldPublication.key == key && oldPublication.token == token <;>
            simpa [mapped, h] using hToken
        · by_cases h : oldPublication.key == key && oldPublication.token == token <;>
            simpa [mapped, h] using hRtd
      · exact hAccounting'
  | beginWarmRead hSnapshot hPublication hLive hCanonical hNoReader hBoundWarm =>
      rename_i binding publication readerId key
      let newRead : WarmRead :=
        { id := readerId, key := binding.key, token := binding.token,
          rtdKey := publication.rtdKey }
      have hWarmReaders' :
          (s.warmReads ++ [newRead]).Pairwise
              (fun lhs rhs => lhs.id ≠ rhs.id) := by
        apply warm_pairwise_append hWarmReaders
        intro read hRead
        exact no_reader_id hNoReader hRead
      have hWarmReaders'' :
          (State.mk s.topics s.publications s.snapshot
            (s.warmReads ++
              [{ id := readerId, key := binding.key, token := binding.token,
                 rtdKey := publication.rtdKey }])).WarmReadersUnique := by
        simpa [newRead, State.WarmReadersUnique] using hWarmReaders'
      refine ⟨hTopics, hPublications, hSnapshots, hWarmReaders'', hLivePublications,
        hProvisionalPublications, hLiveSnapshots, hSnapshotRoots, ?_, ?_⟩
      · intro read hRead
        simp only [List.mem_append, List.mem_singleton] at hRead
        cases hRead with
        | inl hOld => exact hWarmKnown read hOld
        | inr hNew =>
            subst hNew
            exact ⟨publication, mem_publication_of_find hPublication,
              publication_key_of_find hPublication,
              publication_token_of_find hPublication, rfl⟩
      · dsimp [State.PrepareAccounting]
        simpa [Nat.add_assoc, Nat.add_comm, Nat.add_left_comm] using
          (Nat.succ_le_of_lt hBoundWarm)
  | finishWarmRead hRead hPublication hLive hRtdKey =>
      rename_i read0 publication0 readerId0
      have hWarmReaders' :
          (s.warmReads.filter (fun read => read.id != readerId0)).Pairwise
            (fun lhs rhs => lhs.id ≠ rhs.id) :=
        warm_pairwise_filter hWarmReaders (fun read => read.id != readerId0)
      have hWarmKnown' :
          ∀ read ∈ s.removeWarmRead readerId0,
            ∃ publication ∈ s.publications,
              publication.key = read.key ∧ publication.token = read.token ∧
              publication.rtdKey = read.rtdKey := by
        intro read hMem
        exact hWarmKnown read (Topics.mem_of_mem_filter_topics hMem)
      refine ⟨hTopics, hPublications, hSnapshots, hWarmReaders', hLivePublications,
        hProvisionalPublications, hLiveSnapshots, hSnapshotRoots, hWarmKnown', ?_⟩
      dsimp [State.PrepareAccounting, State.removeWarmRead]
      exact Nat.le_trans
        (Nat.add_le_add_right filter_length_le s.topics.runtime.initializers.length)
        hBound

  | disconnect hTopic hTopicKey hTopicOwner hPublication hDestroy =>
      rename_i topics' topic publication0 key owner
      have hTopics' := Topics.DestructionStep.invariant_preserved hTopics hDestroy
      have hPublications' :
          (s.updatePublicationState key topic.token .stale).Pairwise
            (fun lhs rhs => lhs.key ≠ rhs.key ∨ lhs.token ≠ rhs.token) := by
        apply publication_pairwise_map_identity hPublications
        · intro publication
          split <;> rfl
        · intro publication
          split <;> rfl
      have hSnapshots' :
          (s.removeSnapshotIdentity key topic.token).Pairwise
            (fun lhs rhs => lhs.key ≠ rhs.key) := by
        exact snapshot_pairwise_filter hSnapshots _
      have hLiveSnapshots' :
          ∀ binding ∈ s.removeSnapshotIdentity key topic.token,
            ∃ publication ∈ s.updatePublicationState key topic.token .stale,
              publication.state = .live ∧
              publication.key = binding.key ∧
              publication.token = binding.token ∧
              (State.mk topics' (s.updatePublicationState key topic.token .stale)
                (s.removeSnapshotIdentity key topic.token) s.warmReads).CanonicalTopicFor
                publication := by
        intro binding hBinding
        rcases List.mem_filter.mp hBinding with ⟨hOldBinding, hKeep⟩
        rcases hLiveSnapshots binding hOldBinding with
          ⟨old, hOld, hOldLive, hBindingKey, hBindingToken, hOldCanonical⟩
        have hBindingDifferent : binding.key ≠ key ∨ binding.token ≠ topic.token := by
          by_cases hBindingKeyNe : binding.key = key
          · right
            intro hBindingTokenEq
            have hFalse :
                (binding.key != key || binding.token != topic.token) = false := by
              simp [hBindingKeyNe, hBindingTokenEq]
            rw [hFalse] at hKeep
            contradiction
          · exact Or.inl hBindingKeyNe
        have hOldDifferent : old.key ≠ key ∨ old.token ≠ topic.token := by
          by_cases hOldKey : old.key = key
          · exact Or.inr (by
              intro hOldToken
              rcases hBindingDifferent with hBindingKeyNe | hBindingTokenNe
              · exact hBindingKeyNe (hBindingKey.symm.trans hOldKey)
              · exact hBindingTokenNe (hBindingToken.symm.trans hOldToken))
          · exact Or.inl hOldKey
        have hOldKeyNe := canonical_key_ne_provisional_topic
          hTopics hTopic hTopicKey hOldCanonical hOldDifferent
        have hOldMem' : old ∈ s.updatePublicationState key topic.token .stale := by
          apply List.mem_map.mpr
          refine ⟨old, hOld, ?_⟩
          simp [hOldKeyNe]
        have hCanonical := canonical_stage_after_disconnect_non_target
          hDestroy hOldCanonical hOldKeyNe
        exact ⟨old, hOldMem', hOldLive, hBindingKey, hBindingToken, hCanonical⟩
      refine ⟨hTopics', hPublications', hSnapshots', hWarmReaders, ?_, ?_,
        ?_, ?_, ?_, ?_⟩
      · intro publication hMem hLive
        rcases mem_update_publication_identity hMem with
          ⟨old, hOld, hKey, hToken, hRtd, hState⟩
        have hTarget :
            (old.key == key && old.token == topic.token) = false := by
          by_cases h : old.key == key && old.token == topic.token
          · have hStale : publication.state = .stale := by
              simpa [h] using hState
            exact False.elim (by cases hLive.symm.trans hStale)
          · simp [h]
        have hOldLive : old.state = .live := by
          have hState' : publication.state = old.state := by
            simpa [hTarget] using hState
          exact hState'.symm.trans hLive
        have hOldCanonical := hLivePublications old hOld hOldLive
        have hDifferent : old.key ≠ key ∨ old.token ≠ topic.token := by
          by_cases hOldKey : old.key = key
          · exact Or.inr (by
              intro hOldToken
              have hTrue :
                  (old.key == key && old.token == topic.token) = true := by
                simp [hOldKey, hOldToken]
              rw [hTrue] at hTarget
              contradiction)
          · exact Or.inl hOldKey
        have hOldKeyNe := canonical_key_ne_provisional_topic
          hTopics hTopic hTopicKey hOldCanonical hDifferent
        have hCanonical := canonical_stage_after_disconnect_non_target
          hDestroy hOldCanonical hOldKeyNe
        rcases hCanonical with
          ⟨newTopic, hNewMem, hNewKey, hNewToken, hNewRtd, hNewStage⟩
        exact ⟨newTopic, hNewMem, hNewKey.trans hKey.symm,
          hNewToken.trans hToken.symm, hNewRtd.trans hRtd.symm, hNewStage⟩
      · intro publication hMem hProvisional
        rcases mem_update_publication_identity hMem with
          ⟨old, hOld, hKey, hToken, hRtd, hState⟩
        have hTarget :
            (old.key == key && old.token == topic.token) = false := by
          by_cases h : old.key == key && old.token == topic.token
          · have hStale : publication.state = .stale := by
              simpa [h] using hState
            exact False.elim (by cases hProvisional.symm.trans hStale)
          · simp [h]
        have hOldProvisional : old.state = .provisional := by
          have hState' : publication.state = old.state := by
            simpa [hTarget] using hState
          exact hState'.symm.trans hProvisional
        have hOldCanonical := hProvisionalPublications old hOld hOldProvisional
        have hDifferent : old.key ≠ key ∨ old.token ≠ topic.token := by
          by_cases hOldKey : old.key = key
          · exact Or.inr (by
              intro hOldToken
              have hTrue :
                  (old.key == key && old.token == topic.token) = true := by
                simp [hOldKey, hOldToken]
              rw [hTrue] at hTarget
              contradiction)
          · exact Or.inl hOldKey
        have hOldKeyNe := canonical_key_ne_provisional_topic
          hTopics hTopic hTopicKey hOldCanonical hDifferent
        have hCanonical := canonical_stage_after_disconnect_non_target
          hDestroy hOldCanonical hOldKeyNe
        rcases hCanonical with
          ⟨newTopic, hNewMem, hNewKey, hNewToken, hNewRtd, hNewStage⟩
        exact ⟨newTopic, hNewMem, hNewKey.trans hKey.symm,
          hNewToken.trans hToken.symm, hNewRtd.trans hRtd.symm, hNewStage⟩
      · exact hLiveSnapshots'
      · exact live_snapshot_root_of_sound hTopics' hLiveSnapshots'
      · exact warm_known_after_publication_update hWarmKnown
      · cases hDestroy
        exact hBound

  | detachGeneration hDestroy =>
      rename_i topics' generation
      have hTopics' := Topics.DestructionStep.invariant_preserved hTopics hDestroy
      have hPublications' :
          (s.updateGenerationPublications generation).Pairwise
            (fun lhs rhs => lhs.key ≠ rhs.key ∨ lhs.token ≠ rhs.token) := by
        apply publication_pairwise_map_identity hPublications
        · intro publication
          split <;> rfl
        · intro publication
          split <;> rfl
      have hSnapshots' :
          (s.removeGenerationSnapshots generation).Pairwise
            (fun lhs rhs => lhs.key ≠ rhs.key) := by
        exact snapshot_pairwise_filter hSnapshots _
      have hLiveSnapshots' :
          ∀ binding ∈ s.removeGenerationSnapshots generation,
            ∃ publication ∈ s.updateGenerationPublications generation,
              publication.state = .live ∧
              publication.key = binding.key ∧
              publication.token = binding.token ∧
              (State.mk topics' (s.updateGenerationPublications generation)
                (s.removeGenerationSnapshots generation) s.warmReads).CanonicalTopicFor
                publication := by
        intro binding hBinding
        rcases List.mem_filter.mp hBinding with ⟨hOldBinding, hKeep⟩
        rcases hLiveSnapshots binding hOldBinding with
          ⟨old, hOld, hOldLive, hBindingKey, hBindingToken, hOldCanonical⟩
        have hAnyFalse :
            s.topics.byKey.any (fun topic =>
              topic.key == binding.key && topic.token == binding.token &&
                topic.serverGeneration == some generation) = false := by
          simpa using hKeep
        have hTarget :
            s.topics.byKey.any (fun topic =>
              topic.key == old.key && topic.token == old.token &&
                topic.serverGeneration == some generation) = false := by
          simpa [hBindingKey, hBindingToken] using hAnyFalse
        have hOldMem' : old ∈ s.updateGenerationPublications generation := by
          apply List.mem_map.mpr
          refine ⟨old, hOld, ?_⟩
          simp [hTarget]
        have hNotGeneration : ∀ topic ∈ s.topics.byKey,
            topic.key = old.key → topic.token = old.token →
            topic.serverGeneration ≠ some generation := by
          intro target hTargetMem hTargetKey hTargetToken hGeneration
          have hAny :
              s.topics.byKey.any (fun topic =>
                topic.key == old.key && topic.token == old.token &&
                  topic.serverGeneration == some generation) = true := by
            apply List.any_eq_true.mpr
            exact ⟨target, hTargetMem, by simp [hTargetKey, hTargetToken, hGeneration]⟩
          rw [hAny] at hTarget
          contradiction
        have hCanonical := canonical_stage_after_detach_non_target
          hTopics hDestroy hOldCanonical hNotGeneration
        exact ⟨old, hOldMem', hOldLive, hBindingKey, hBindingToken, hCanonical⟩
      refine ⟨hTopics', hPublications', hSnapshots', hWarmReaders, ?_, ?_,
        ?_, ?_, ?_, ?_⟩
      · intro publication hMem hLive
        rcases mem_update_generation_publication_identity hMem with
          ⟨old, hOld, hKey, hToken, hRtd, hState⟩
        have hTarget :
            s.topics.byKey.any (fun topic =>
              topic.key == old.key && topic.token == old.token &&
                topic.serverGeneration == some generation) = false := by
          by_cases h : s.topics.byKey.any (fun topic =>
              topic.key == old.key && topic.token == old.token &&
                topic.serverGeneration == some generation)
          · have hStale : publication.state = .stale := by
              simpa [h] using hState
            exact False.elim (by cases hLive.symm.trans hStale)
          · simp [h]
        have hOldLive : old.state = .live := by
          have hState' : publication.state = old.state := by
            simpa [hTarget] using hState
          exact hState'.symm.trans hLive
        have hOldCanonical := hLivePublications old hOld hOldLive
        have hNotGeneration : ∀ topic ∈ s.topics.byKey,
            topic.key = old.key → topic.token = old.token →
            topic.serverGeneration ≠ some generation := by
          intro target hTargetMem hTargetKey hTargetToken hGeneration
          have hAny :
              s.topics.byKey.any (fun topic =>
                topic.key == old.key && topic.token == old.token &&
                  topic.serverGeneration == some generation) = true := by
            apply List.any_eq_true.mpr
            exact ⟨target, hTargetMem, by simp [hTargetKey, hTargetToken, hGeneration]⟩
          rw [hAny] at hTarget
          contradiction
        have hCanonical := canonical_stage_after_detach_non_target
          hTopics hDestroy hOldCanonical hNotGeneration
        rcases hCanonical with
          ⟨newTopic, hNewMem, hNewKey, hNewToken, hNewRtd, hNewStage⟩
        exact ⟨newTopic, hNewMem, hNewKey.trans hKey.symm,
          hNewToken.trans hToken.symm, hNewRtd.trans hRtd.symm, hNewStage⟩
      · intro publication hMem hProvisional
        rcases mem_update_generation_publication_identity hMem with
          ⟨old, hOld, hKey, hToken, hRtd, hState⟩
        have hTarget :
            s.topics.byKey.any (fun topic =>
              topic.key == old.key && topic.token == old.token &&
                topic.serverGeneration == some generation) = false := by
          by_cases h : s.topics.byKey.any (fun topic =>
              topic.key == old.key && topic.token == old.token &&
                topic.serverGeneration == some generation)
          · have hStale : publication.state = .stale := by
              simpa [h] using hState
            exact False.elim (by cases hProvisional.symm.trans hStale)
          · simp [h]
        have hOldProvisional : old.state = .provisional := by
          have hState' : publication.state = old.state := by
            simpa [hTarget] using hState
          exact hState'.symm.trans hProvisional
        have hOldCanonical := hProvisionalPublications old hOld hOldProvisional
        have hNotGeneration : ∀ topic ∈ s.topics.byKey,
            topic.key = old.key → topic.token = old.token →
            topic.serverGeneration ≠ some generation := by
          intro target hTargetMem hTargetKey hTargetToken hGeneration
          have hAny :
              s.topics.byKey.any (fun topic =>
                topic.key == old.key && topic.token == old.token &&
                  topic.serverGeneration == some generation) = true := by
            apply List.any_eq_true.mpr
            exact ⟨target, hTargetMem, by simp [hTargetKey, hTargetToken, hGeneration]⟩
          rw [hAny] at hTarget
          contradiction
        have hCanonical := canonical_stage_after_detach_non_target
          hTopics hDestroy hOldCanonical hNotGeneration
        rcases hCanonical with
          ⟨newTopic, hNewMem, hNewKey, hNewToken, hNewRtd, hNewStage⟩
        exact ⟨newTopic, hNewMem, hNewKey.trans hKey.symm,
          hNewToken.trans hToken.symm, hNewRtd.trans hRtd.symm, hNewStage⟩
      · exact hLiveSnapshots'
      · exact live_snapshot_root_of_sound hTopics' hLiveSnapshots'
      · exact warm_known_after_generation_update hWarmKnown
      · cases hDestroy
        exact hBound

  | drainPendingReuse hDestroy =>
      rename_i topics' token runtimeId nextGeneration
      have hTopics' := Topics.DestructionStep.invariant_preserved hTopics hDestroy
      have hLive' : ∀ publication ∈ s.publications,
          publication.state = .live →
          (State.mk topics' s.publications s.snapshot s.warmReads).CanonicalTopicFor
            publication := by
        intro publication hPublication hLive
        exact canonical_stage_after_pending_reuse hDestroy
          (hLivePublications publication hPublication hLive)
      have hProvisional' : ∀ publication ∈ s.publications,
          publication.state = .provisional →
          (State.mk topics' s.publications s.snapshot s.warmReads).CanonicalTopicForStage
            publication .provisional := by
        intro publication hPublication hProvisional
        exact canonical_stage_after_pending_reuse hDestroy
          (hProvisionalPublications publication hPublication hProvisional)
      have hSnapshots' :
          ∀ binding ∈ s.snapshot,
            ∃ publication ∈ s.publications,
              publication.state = .live ∧
              publication.key = binding.key ∧
              publication.token = binding.token ∧
              (State.mk topics' s.publications s.snapshot s.warmReads).CanonicalTopicFor
                publication := by
        intro binding hBinding
        rcases hLiveSnapshots binding hBinding with
          ⟨publication, hPublication, hLive, hKey, hToken, hCanonical⟩
        exact ⟨publication, hPublication, hLive, hKey, hToken,
          canonical_stage_after_pending_reuse hDestroy hCanonical⟩
      have hRoots' := live_snapshot_root_of_sound
        (s := State.mk topics' s.publications s.snapshot s.warmReads)
        hTopics' hSnapshots'
      refine ⟨hTopics', hPublications, hSnapshots, hWarmReaders, hLive',
        hProvisional', hSnapshots', hRoots', hWarmKnown, ?_⟩
      cases hDestroy with
      | drainPendingReuse hDetached hInit hPending hRuntime =>
          cases hRuntime
          simpa [State.PrepareAccounting, Runtime.State.updateInitializer] using hBound

  | drainPendingRetire hDestroy =>
      rename_i topics' token runtimeId
      have hTopics' := Topics.DestructionStep.invariant_preserved hTopics hDestroy
      have hLive' : ∀ publication ∈ s.publications,
          publication.state = .live →
          (State.mk topics' s.publications s.snapshot s.warmReads).CanonicalTopicFor
            publication := by
        intro publication hPublication hLive
        exact canonical_stage_after_pending_retire hDestroy
          (hLivePublications publication hPublication hLive)
      have hProvisional' : ∀ publication ∈ s.publications,
          publication.state = .provisional →
          (State.mk topics' s.publications s.snapshot s.warmReads).CanonicalTopicForStage
            publication .provisional := by
        intro publication hPublication hProvisional
        exact canonical_stage_after_pending_retire hDestroy
          (hProvisionalPublications publication hPublication hProvisional)
      have hSnapshots' :
          ∀ binding ∈ s.snapshot,
            ∃ publication ∈ s.publications,
              publication.state = .live ∧
              publication.key = binding.key ∧
              publication.token = binding.token ∧
              (State.mk topics' s.publications s.snapshot s.warmReads).CanonicalTopicFor
                publication := by
        intro binding hBinding
        rcases hLiveSnapshots binding hBinding with
          ⟨publication, hPublication, hLive, hKey, hToken, hCanonical⟩
        exact ⟨publication, hPublication, hLive, hKey, hToken,
          canonical_stage_after_pending_retire hDestroy hCanonical⟩
      have hRoots' := live_snapshot_root_of_sound
        (s := State.mk topics' s.publications s.snapshot s.warmReads)
        hTopics' hSnapshots'
      refine ⟨hTopics', hPublications, hSnapshots, hWarmReaders, hLive',
        hProvisional', hSnapshots', hRoots', hWarmKnown, ?_⟩
      cases hDestroy with
      | drainPendingRetire hDetached hInit hPending hRuntime =>
          cases hRuntime
          simpa [State.PrepareAccounting, Runtime.State.updateInitializer] using hBound

  | drainPublishedReuse hDestroy =>
      rename_i topics' token nextGeneration
      have hTopics' := Topics.DestructionStep.invariant_preserved hTopics hDestroy
      have hLive' : ∀ publication ∈ s.publications,
          publication.state = .live →
          (State.mk topics' s.publications s.snapshot s.warmReads).CanonicalTopicFor
            publication := by
        intro publication hPublication hLive
        exact canonical_stage_after_published_reuse hDestroy
          (hLivePublications publication hPublication hLive)
      have hProvisional' : ∀ publication ∈ s.publications,
          publication.state = .provisional →
          (State.mk topics' s.publications s.snapshot s.warmReads).CanonicalTopicForStage
            publication .provisional := by
        intro publication hPublication hProvisional
        exact canonical_stage_after_published_reuse hDestroy
          (hProvisionalPublications publication hPublication hProvisional)
      have hSnapshots' :
          ∀ binding ∈ s.snapshot,
            ∃ publication ∈ s.publications,
              publication.state = .live ∧
              publication.key = binding.key ∧
              publication.token = binding.token ∧
              (State.mk topics' s.publications s.snapshot s.warmReads).CanonicalTopicFor
                publication := by
        intro binding hBinding
        rcases hLiveSnapshots binding hBinding with
          ⟨publication, hPublication, hLive, hKey, hToken, hCanonical⟩
        exact ⟨publication, hPublication, hLive, hKey, hToken,
          canonical_stage_after_published_reuse hDestroy hCanonical⟩
      have hRoots' := live_snapshot_root_of_sound
        (s := State.mk topics' s.publications s.snapshot s.warmReads)
        hTopics' hSnapshots'
      refine ⟨hTopics', hPublications, hSnapshots, hWarmReaders, hLive',
        hProvisional', hSnapshots', hRoots', hWarmKnown, ?_⟩
      cases hDestroy
      exact hBound

  | drainPublishedRetire hDestroy =>
      rename_i topics' token
      have hTopics' := Topics.DestructionStep.invariant_preserved hTopics hDestroy
      have hLive' : ∀ publication ∈ s.publications,
          publication.state = .live →
          (State.mk topics' s.publications s.snapshot s.warmReads).CanonicalTopicFor
            publication := by
        intro publication hPublication hLive
        exact canonical_stage_after_published_retire hDestroy
          (hLivePublications publication hPublication hLive)
      have hProvisional' : ∀ publication ∈ s.publications,
          publication.state = .provisional →
          (State.mk topics' s.publications s.snapshot s.warmReads).CanonicalTopicForStage
            publication .provisional := by
        intro publication hPublication hProvisional
        exact canonical_stage_after_published_retire hDestroy
          (hProvisionalPublications publication hPublication hProvisional)
      have hSnapshots' :
          ∀ binding ∈ s.snapshot,
            ∃ publication ∈ s.publications,
              publication.state = .live ∧
              publication.key = binding.key ∧
              publication.token = binding.token ∧
              (State.mk topics' s.publications s.snapshot s.warmReads).CanonicalTopicFor
                publication := by
        intro binding hBinding
        rcases hLiveSnapshots binding hBinding with
          ⟨publication, hPublication, hLive, hKey, hToken, hCanonical⟩
        exact ⟨publication, hPublication, hLive, hKey, hToken,
          canonical_stage_after_published_retire hDestroy hCanonical⟩
      have hRoots' := live_snapshot_root_of_sound
        (s := State.mk topics' s.publications s.snapshot s.warmReads)
        hTopics' hSnapshots'
      refine ⟨hTopics', hPublications, hSnapshots, hWarmReaders, hLive',
        hProvisional', hSnapshots', hRoots', hWarmKnown, ?_⟩
      cases hDestroy
      exact hBound

  | closeRegistry hNoWarmReads hNoSnapshot hTopicsStep =>
      rename_i topics'
      have hTopics' := Topics.Step.invariant_preserved hTopics hTopicsStep
      cases hTopicsStep with
      | closeRegistry hNoVisible hNoReverse hNoExcelOwners hNoInitializers
          hNoDetached hRuntime =>
          refine ⟨hTopics', hPublications, hSnapshots, hWarmReaders, ?_, ?_,
            ?_, ?_, ?_, ?_⟩
          · intro publication hMem hLive
            rcases hLivePublications publication hMem hLive with
              ⟨topic, hTopic, hKey, hToken, hRtd, hStage⟩
            rw [hNoVisible] at hTopic
            contradiction
          · intro publication hMem hProvisional
            rcases hProvisionalPublications publication hMem hProvisional with
              ⟨topic, hTopic, hKey, hToken, hRtd, hStage⟩
            rw [hNoVisible] at hTopic
            contradiction
          · intro binding hBinding
            rw [hNoSnapshot] at hBinding
            contradiction
          · intro binding hBinding
            rw [hNoSnapshot] at hBinding
            contradiction
          · intro read hRead
            rw [hNoWarmReads] at hRead
            contradiction
          · cases hRuntime
            simp_all [State.PrepareAccounting]

  | sealForClose hTopicsStep =>
      rename_i topics'
      have hTopics' := Topics.Step.invariant_preserved hTopics hTopicsStep
      have hPublications' :
          (s.updateClosingPublications).Pairwise
            (fun lhs rhs => lhs.key ≠ rhs.key ∨ lhs.token ≠ rhs.token) := by
        apply publication_pairwise_map_identity hPublications
        · intro publication
          rfl
        · intro publication
          rfl
      have hLive' : ∀ publication ∈ s.updateClosingPublications,
          publication.state = .live →
          (State.mk topics' s.updateClosingPublications [] s.warmReads).CanonicalTopicFor
            publication := by
        intro publication hMem hLive
        rcases mem_update_closing_publication_identity hMem with
          ⟨old, hOld, hKey, hToken, hRtd, hState⟩
        cases hOldState : old.state with
        | provisional =>
            have hState' : publication.state = .closing := by
              simpa [closingState, hOldState] using hState
            exact False.elim (by cases hLive.symm.trans hState')
        | live =>
            have hState' : publication.state = .closing := by
              simpa [closingState, hOldState] using hState
            exact False.elim (by cases hLive.symm.trans hState')
        | stale =>
            have hState' : publication.state = .stale := by
              simpa [closingState, hOldState] using hState
            exact False.elim (by cases hLive.symm.trans hState')
        | closing =>
            have hState' : publication.state = .closing := by
              simpa [closingState, hOldState] using hState
            exact False.elim (by cases hLive.symm.trans hState')
      have hProvisional' : ∀ publication ∈ s.updateClosingPublications,
          publication.state = .provisional →
          (State.mk topics' s.updateClosingPublications [] s.warmReads).CanonicalTopicForStage
            publication .provisional := by
        intro publication hMem hProvisional
        rcases mem_update_closing_publication_identity hMem with
          ⟨old, hOld, hKey, hToken, hRtd, hState⟩
        cases hOldState : old.state with
        | provisional =>
            have hState' : publication.state = .closing := by
              simpa [closingState, hOldState] using hState
            exact False.elim (by cases hProvisional.symm.trans hState')
        | live =>
            have hState' : publication.state = .closing := by
              simpa [closingState, hOldState] using hState
            exact False.elim (by cases hProvisional.symm.trans hState')
        | stale =>
            have hState' : publication.state = .stale := by
              simpa [closingState, hOldState] using hState
            exact False.elim (by cases hProvisional.symm.trans hState')
        | closing =>
            have hState' : publication.state = .closing := by
              simpa [closingState, hOldState] using hState
            exact False.elim (by cases hProvisional.symm.trans hState')
      have hWarmKnown' := warm_known_after_closing_update hWarmKnown
      refine ⟨hTopics', hPublications', List.Pairwise.nil, hWarmReaders, hLive',
        hProvisional', ?_, ?_, hWarmKnown', ?_⟩
      · intro binding hBinding
        contradiction
      · intro binding hBinding
        contradiction
      · cases hTopicsStep with
        | sealTopics hRuntime =>
            cases hRuntime
            exact hBound

  | failWarmRead hRead hPublication hLive hRtdKey =>
      rename_i read0 publication0 readerId0
      have hWarmReaders' :
          (s.warmReads.filter (fun read => read.id != readerId0)).Pairwise
            (fun lhs rhs => lhs.id ≠ rhs.id) :=
        warm_pairwise_filter hWarmReaders (fun read => read.id != readerId0)
      have hWarmKnown' :
          ∀ read ∈ s.removeWarmRead readerId0,
            ∃ publication ∈ s.publications,
              publication.key = read.key ∧ publication.token = read.token ∧
              publication.rtdKey = read.rtdKey := by
        intro read hMem
        exact hWarmKnown read (Topics.mem_of_mem_filter_topics hMem)
      refine ⟨hTopics, hPublications, hSnapshots, hWarmReaders', hLivePublications,
        hProvisionalPublications, hLiveSnapshots, hSnapshotRoots, hWarmKnown', ?_⟩
      dsimp [State.PrepareAccounting, State.removeWarmRead]
      exact Nat.le_trans
        (Nat.add_le_add_right filter_length_le s.topics.runtime.initializers.length)
        hBound

  | abandonWarmRead hRead hPublication hInvalidated hRtdKey =>
      rename_i read0 publication0 readerId0
      have hWarmReaders' :
          (s.warmReads.filter (fun read => read.id != readerId0)).Pairwise
            (fun lhs rhs => lhs.id ≠ rhs.id) :=
        warm_pairwise_filter hWarmReaders (fun read => read.id != readerId0)
      have hWarmKnown' :
          ∀ read ∈ s.removeWarmRead readerId0,
            ∃ publication ∈ s.publications,
              publication.key = read.key ∧ publication.token = read.token ∧
              publication.rtdKey = read.rtdKey := by
        intro read hMem
        exact hWarmKnown read (Topics.mem_of_mem_filter_topics hMem)
      refine ⟨hTopics, hPublications, hSnapshots, hWarmReaders', hLivePublications,
        hProvisionalPublications, hLiveSnapshots, hSnapshotRoots, hWarmKnown', ?_⟩
      dsimp [State.PrepareAccounting, State.removeWarmRead]
      exact Nat.le_trans
        (Nat.add_le_add_right filter_length_le s.topics.runtime.initializers.length)
        hBound

inductive Reachable : State → State → Prop where
  | refl (s : State) : Reachable s s
  | tail {s t u : State} {event : Event} :
      Reachable s t → Step t event u → Reachable s u

theorem Reachable.invariant_preserved
    {s t : State}
    (hInv : s.Invariant)
    (hReach : Reachable s t) :
    t.Invariant := by
  induction hReach with
  | refl => exact hInv
  | tail hReach hStep ih =>
      exact Step.invariant_preserved ih hStep

theorem reachable_invariant
    {topics : Topics.State} {s : State}
    (hTopics : topics.Invariant)
    (hReach : Reachable (initialState topics) s) :
    s.Invariant := by
  exact Reachable.invariant_preserved (initialInvariant hTopics) hReach

end XlFnFormal.Handle.Refinement
