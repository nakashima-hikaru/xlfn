import XlFnFormal.Handle.Refinement.PublishedChecker

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Refinement

open XlFnFormal.Handle.Topics

theorem successful_warm_read_is_abstractly_valid
    {s s' : State} {readerId : Nat} {read : WarmRead} {publication : Publication}
    (hInv : s.Invariant)
    (hRead : s.findWarmRead? readerId = some read)
    (hPublication : s.findPublication? read.key read.token = some publication)
    (hStep : Step s (.finishWarmRead readerId) s') :
    publication.state = .live ∧
      s.CanonicalTopicFor publication := by
  unfold Step apply? at hStep
  have hStep' :
      (publication.state = .live ∧ publication.rtdKey = read.rtdKey) ∧
        { s with warmReads := s.removeWarmRead readerId } = s' := by
    simpa [hRead, hPublication] using hStep
  have hPublicationMem : publication ∈ s.publications := by
    dsimp [State.findPublication?] at hPublication
    exact Runtime.List.mem_of_find?_eq_some' hPublication
  have hLive : publication.state = .live := hStep'.1.1
  have hSound := hInv.2.2.2.2.1 publication hPublicationMem hLive
  exact ⟨hLive, hSound⟩

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
  unfold Step apply? at hStep
  have hStep' :
      (publication.state = .live ∧ publication.rtdKey = read.rtdKey) ∧
        { s with warmReads := s.removeWarmRead readerId } = s' := by
    simpa [hRead, hPublication] using hStep
  have hLive : publication.state = .live := hStep'.1.1
  rcases hInvalidated with hStale | hClosing
  · cases hLive.symm.trans hStale
  · cases hLive.symm.trans hClosing

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
    {s s' : State} {readerId : Nat} {read : WarmRead}
    {publication : Publication} {generation : ServerGeneration}
    (hRead : s.findWarmRead? readerId = some read)
    (hPublication : s.findPublication? read.key read.token = some publication)
    (hGeneration : ∃ topic ∈ s.topics.byKey,
      topic.key = publication.key ∧
      topic.token = publication.token ∧
      topic.serverGeneration = some generation)
    (hInvalidated : publication.state = .stale)
    (hStep : Step s (.finishWarmRead readerId) s') : False := by
  exact invalidated_warm_reader_cannot_succeed hRead hPublication
    (Or.inl hInvalidated) hStep

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
  exact invalidated_warm_reader_cannot_succeed hRead hOld
    (Or.inl hOldStale) hStep

theorem registry_close_follows_warm_read_drain
    {s s' : State}
    (hStep : Step s .registryClose s') :
    s.warmReads = [] := by
  unfold Step apply? at hStep
  by_cases hEmpty : s.warmReads = []
  · exact hEmpty
  · simp [hEmpty] at hStep

end XlFnFormal.Handle.Refinement
