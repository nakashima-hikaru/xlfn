import XlFnFormal.Handle.Topics.Checker

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Topics

def orphanKey : TopicKey :=
  { sheetId := 0, row := 0, column := 0, udfId := "orphan", argumentDigest := 0 }

def orphanRtdKey : RtdKey := "orphan-rtd"

def orphanProvisional (session : Registry.SessionId) : State :=
  let token : Registry.Token := { session := session, slot := 0, generation := 1 }
  { runtime := Runtime.initialState session
    byKey :=
      [{ key := orphanKey, rtdKey := orphanRtdKey, token := token, stage := .provisional,
         serverGeneration := none, excelOwner := none, excelCommitted := false }]
    byRtdKey := []
    byExcelOwner := []
    initializing := []
    detached := [] }

/-! A visible topic without an initializer owner is not enough to authorize a
    commit.  This is the malformed state the H3.1 provenance gate excludes. -/
theorem orphan_provisional_cannot_commit (session : Registry.SessionId) :
    ¬ ∃ s', Step (orphanProvisional session)
      (.commitPublication orphanKey 1) s' := by
  intro hExists
  rcases hExists with ⟨s', hStep⟩
  cases hStep with
  | commitPublication hInit hTopic hTopicKey hExcelSettled hPending hRuntime =>
      dsimp [orphanProvisional, State.findInitializing?] at hInit
      contradiction

theorem provisional_topic_cannot_finish_before_commit
    {s s' : State} {key : TopicKey} {runtimeId : Runtime.InitializerId}
    {topic : Topic}
    (hTopic : topic ∈ s.byKey)
    (hTopicKey : topic.key = key)
    (hProvisional : topic.stage = .provisional)
    (hStep : Step s (.finishInitializer key runtimeId) s') :
    False := by
  cases hStep with
  | finishInitializer hInit hReady hRuntime =>
      have hCommitted := hReady topic hTopic hTopicKey
      rw [hProvisional] at hCommitted
      contradiction

theorem commit_after_seal_is_rejected
    {s : State} {key : TopicKey} {runtimeId : Runtime.InitializerId}
    (hSealed : s.runtime.phase = .drainingPrepares) :
    ¬ ∃ s', Step s (.commitPublication key runtimeId) s' := by
  intro hExists
  rcases hExists with ⟨s', hStep⟩
  cases hStep with
  | commitPublication hInit hTopic hTopicKey hExcelSettled hPending hRuntime =>
      cases hRuntime with
      | publishTopic hPhase hFind =>
          rw [hSealed] at hPhase
          contradiction

theorem publish_after_seal_is_rejected
    {s : State} {key : TopicKey} {runtimeId : Runtime.InitializerId} {rtdKey : RtdKey}
    (hSealed : s.runtime.phase = .drainingPrepares) :
    ¬ ∃ s', Step s (.publishVisible key runtimeId rtdKey) s' := by
  exact no_topic_publication_after_seal hSealed

end XlFnFormal.Handle.Topics
