import XlFnFormal.Handle.Refinement.PublishedChecker

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Refinement

open XlFnFormal.Handle.Topics

private theorem publication_mem_of_find
    {s : State} {key : TopicKey} {token : Registry.Token}
    {publication : Publication}
    (hFind : s.findPublication? key token = some publication) :
    publication ∈ s.publications := by
  dsimp [State.findPublication?] at hFind
  exact Runtime.List.mem_of_find?_eq_some' hFind

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

theorem successful_warm_read_is_abstractly_valid
    {s s' : State} {readerId : Nat} {read : WarmRead}
    {publication : Publication}
    (hInv : s.Invariant)
    (hRead : s.findWarmRead? readerId = some read)
    (hPublication : s.findPublication? read.key read.token = some publication)
    (hStep : Step s (.finishWarmRead readerId) s') :
    publication.state = .live ∧
      s.CanonicalTopicFor publication := by
  cases hStep with
  | finishWarmRead hReadStep hPublicationStep hLive hRtdKey =>
      rename_i readStep publicationStep
      have hReadEq : read = readStep :=
        (Option.some.inj (hReadStep.symm.trans hRead)).symm
      cases hReadEq
      have hPublicationEq : publication = publicationStep :=
        (Option.some.inj (hPublicationStep.symm.trans hPublication)).symm
      cases hPublicationEq
      have hMem := publication_mem_of_find hPublicationStep
      exact ⟨hLive, hInv.2.2.2.2.1 publication hMem hLive⟩

theorem successful_warm_read_root_is_live
    {s : State} {publication : Publication}
    (hTopics : s.topics.Invariant)
    (hPublication : publication ∈ s.publications)
    (hLive : publication.state = .live)
    (hCanonical : s.CanonicalTopicFor publication) :
    ∃ topic ∈ s.topics.byKey,
      topic.key = publication.key ∧
      topic.token = publication.token ∧
      topic.rtdKey = publication.rtdKey ∧
      Runtime.TokenLive s.topics.runtime.registry topic.token := by
  rcases hCanonical with ⟨topic, hTopic, hKey, hToken, hRtd, hStage⟩
  refine ⟨topic, hTopic, hKey, hToken, hRtd, ?_⟩
  exact hTopics.2.2.2.2.2.2.2.2.2.2.1 topic hTopic

theorem invalidated_warm_reader_cannot_succeed
    {s s' : State} {readerId : Nat} {read : WarmRead}
    {publication : Publication}
    (hRead : s.findWarmRead? readerId = some read)
    (hPublication : s.findPublication? read.key read.token = some publication)
    (hInvalidated : publication.state = .stale ∨
      publication.state = .closing)
    (hStep : Step s (.finishWarmRead readerId) s') : False := by
  cases hStep with
  | finishWarmRead hReadStep hPublicationStep hLive hRtdKey =>
      rename_i readStep publicationStep
      have hReadEq : read = readStep :=
        (Option.some.inj (hReadStep.symm.trans hRead)).symm
      cases hReadEq
      have hPublicationEq : publication = publicationStep :=
        (Option.some.inj (hPublicationStep.symm.trans hPublication)).symm
      cases hPublicationEq
      rcases hInvalidated with hStale | hClosing
      · exact False.elim (by cases hLive.symm.trans hStale)
      · exact False.elim (by cases hLive.symm.trans hClosing)

theorem closing_warm_reader_cannot_succeed
    {s s' : State} {readerId : Nat} {read : WarmRead}
    {publication : Publication}
    (hRead : s.findWarmRead? readerId = some read)
    (hPublication : s.findPublication? read.key read.token = some publication)
    (hClosing : publication.state = .closing)
    (hStep : Step s (.finishWarmRead readerId) s') : False := by
  exact invalidated_warm_reader_cannot_succeed hRead hPublication
    (Or.inr hClosing) hStep

theorem terminated_generation_warm_reader_cannot_succeed
    {s s' s'' : State} {readerId : Nat} {read : WarmRead}
    {publication : Publication} {generation : ServerGeneration}
    (hDetach : Step s (.detachGeneration generation) s')
    (hRead : s'.findWarmRead? readerId = some read)
    (hPublication : s'.findPublication? read.key read.token = some publication)
    (hGeneration : ∃ topic ∈ s.topics.byKey,
      topic.key = publication.key ∧
      topic.token = publication.token ∧
      topic.serverGeneration = some generation)
    (hStep : Step s' (.finishWarmRead readerId) s'') : False := by
  cases hDetach with
  | detachGeneration hDestroy =>
      rcases hGeneration with ⟨topic, hTopic, hTopicKey, hTopicToken,
        hServerGeneration⟩
      have hPublicationMem := publication_mem_of_find hPublication
      rcases List.mem_map.mp hPublicationMem with ⟨old, hOld, rfl⟩
      have hTopicKey' : topic.key = old.key := by
        by_cases h : s.topics.byKey.any (fun candidate =>
            candidate.key == old.key && candidate.token == old.token &&
              candidate.serverGeneration == some generation) = true <;>
          simpa [h] using hTopicKey
      have hTopicToken' : topic.token = old.token := by
        by_cases h : s.topics.byKey.any (fun candidate =>
            candidate.key == old.key && candidate.token == old.token &&
              candidate.serverGeneration == some generation) = true <;>
          simpa [h] using hTopicToken
      have hAny :
          s.topics.byKey.any (fun candidate =>
            candidate.key == old.key && candidate.token == old.token &&
              candidate.serverGeneration == some generation) = true := by
        apply List.any_eq_true.mpr
        exact ⟨topic, hTopic, by
          simp [hTopicKey', hTopicToken', hServerGeneration]⟩
      have hStale :
          (if s.topics.byKey.any (fun candidate =>
              candidate.key == old.key && candidate.token == old.token &&
                candidate.serverGeneration == some generation) then
            { old with state := .stale }
          else old).state = .stale := by
        simp [hAny]
      exact invalidated_warm_reader_cannot_succeed hRead hPublication
        (Or.inl hStale) hStep

theorem stale_reader_cannot_follow_replacement_publication
    {s s' : State} {readerId : Nat} {read : WarmRead}
    {oldPublication newPublication : Publication}
    (hRead : s.findWarmRead? readerId = some read)
    (hOld : s.findPublication? read.key read.token = some oldPublication)
    (hOldStale : oldPublication.state = .stale)
    (hReplacement : newPublication.key = oldPublication.key ∧
      newPublication.token ≠ oldPublication.token ∧
      newPublication.state = .live)
    (hStep : Step s (.finishWarmRead readerId) s') : False := by
  have hOldToken := publication_token_of_find hOld
  have hReplacementDoesNotMatchRead : newPublication.token ≠ read.token := by
    intro hEq
    exact hReplacement.2.1 (hEq.trans hOldToken.symm)
  by_cases hEq : newPublication.token = read.token
  · exact False.elim (hReplacementDoesNotMatchRead hEq)
  · exact invalidated_warm_reader_cannot_succeed hRead hOld
      (Or.inl hOldStale) hStep

theorem registry_close_follows_warm_read_drain
    {s s' : State}
    (hStep : Step s .closeRegistry s') :
    s.warmReads = [] ∧
      s.snapshot = [] ∧
      s.topics.runtime.activePrepares = 0 ∧
      s.topics.initializing = [] := by
  cases hStep with
  | closeRegistry hNoWarmReads hNoSnapshot hTopics =>
      cases hTopics with
      | closeRegistry hNoVisible hNoReverse hNoExcelOwners hNoInitializers
          hNoDetached hRuntime =>
          cases hRuntime with
          | closeRegistry hPhase hRuntimeInitializers hNoPrepares hRegistry =>
              exact ⟨hNoWarmReads, hNoSnapshot, hNoPrepares,
                hNoInitializers⟩

theorem warm_observation_failure_is_non_destructive
    {s s' : State} {readerId : Nat} {read : WarmRead}
    {publication : Publication}
    (hRead : s.findWarmRead? readerId = some read)
    (hPublication : s.findPublication? read.key read.token = some publication)
    (hStep : Step s (.failWarmRead readerId) s') :
    publication.state = .live ∧
      s'.findPublication? read.key read.token = some publication := by
  cases hStep with
  | failWarmRead hReadStep hPublicationStep hLive hRtdKey =>
      rename_i readStep publicationStep
      have hReadEq : read = readStep :=
        (Option.some.inj (hReadStep.symm.trans hRead)).symm
      cases hReadEq
      have hPublicationEq : publication = publicationStep :=
        (Option.some.inj (hPublicationStep.symm.trans hPublication)).symm
      cases hPublicationEq
      exact ⟨hLive, by simpa [State.findPublication?] using hPublicationStep⟩

end XlFnFormal.Handle.Refinement
