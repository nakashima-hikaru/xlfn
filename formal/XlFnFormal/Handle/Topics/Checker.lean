import XlFnFormal.Handle.Topics.Safety
import XlFnFormal.Handle.Runtime.Checker

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Topics

private def tokenLive? (registry : Registry.State) (token : Registry.Token) : Bool :=
  if hSession : token.session = registry.session then
    match registry.slots[token.slot]? with
    | some (.live generation) => generation = token.generation
    | _ => false
  else false

private theorem tokenLive?_iff
    {registry : Registry.State} {token : Registry.Token} :
    tokenLive? registry token = true ↔ Runtime.TokenLive registry token := by
  unfold tokenLive? Runtime.TokenLive
  by_cases hSession : token.session = registry.session
  · simp only [hSession]
    cases hGet : registry.slots[token.slot]? with
    | none =>
        have hNoBounds := (getElem?_eq_none_iff registry.slots token.slot).mp hGet
        simp [hNoBounds]
    | some slotState =>
        cases slotState with
        | vacant generation =>
            have hGet' := getElem?_eq_some_iff.mp hGet
            rcases hGet' with ⟨hBounds, hSlot⟩
            have hNoLive : ¬∃ h, registry.slots[token.slot] =
                Registry.SlotState.live token.generation := by
              intro hLive
              have hEq : Registry.SlotState.vacant generation =
                  Registry.SlotState.live token.generation := hSlot.symm.trans hLive.choose_spec
              cases hEq
            simp [hNoLive]
        | retired =>
            have hGet' := getElem?_eq_some_iff.mp hGet
            rcases hGet' with ⟨hBounds, hSlot⟩
            have hNoLive : ¬∃ h, registry.slots[token.slot] =
                Registry.SlotState.live token.generation := by
              intro hLive
              have hEq : Registry.SlotState.retired =
                  Registry.SlotState.live token.generation := hSlot.symm.trans hLive.choose_spec
              cases hEq
            simp [hNoLive]
        | live generation =>
            have hGet' := getElem?_eq_some_iff.mp hGet
            rcases hGet' with ⟨hBounds, hSlot⟩
            simp
            constructor
            · intro hEq
              subst generation
              exact ⟨hBounds, hSlot⟩
            · intro hLive
              have hEq : Registry.SlotState.live generation =
                  Registry.SlotState.live token.generation := by
                exact hSlot.symm.trans hLive.choose_spec
              cases hEq
              rfl
  · simp [hSession]

/-! The executable checker keeps the topic table and the Runtime state in lockstep.
    In particular, a visible provisional topic is admitted only when the Runtime
    checker has produced the matching pending initializer root. -/

def apply? (s : State) (event : Event) : Option State :=
  match event with
  | .beginPrepare =>
      match Runtime.apply? s.runtime .beginPrepare with
      | some runtime' => some { s with runtime := runtime' }
      | none => none
  | .endPrepare =>
      match Runtime.apply? s.runtime .endPrepare with
      | some runtime' => some { s with runtime := runtime' }
      | none => none
  | .sealTopics =>
      match Runtime.apply? s.runtime .sealTopics with
      | some runtime' => some { s with runtime := runtime' }
      | none => none
  | .beginLookup token =>
      match Runtime.apply? s.runtime (.beginLookup token) with
      | some runtime' => some { s with runtime := runtime' }
      | none => none
  | .endLookup =>
      match Runtime.apply? s.runtime .endLookup with
      | some runtime' => some { s with runtime := runtime' }
      | none => none
  | .beginInitializer key runtimeId =>
      if s.findTopic? key = none ∧
          s.findInitializing? key = none ∧
          (∀ init ∈ s.initializing, init.runtimeId ≠ runtimeId) then
        match Runtime.apply? s.runtime (.beginInitialize runtimeId) with
        | some runtime' =>
            some { s with
              runtime := runtime'
              initializing := s.initializing ++ [{ runtimeId := runtimeId, key := key }] }
        | none => none
      else none
  | .publishVisibleFresh key runtimeId =>
      match Runtime.apply? s.runtime (.insertPendingFresh runtimeId) with
      | some runtime' =>
          match runtime'.findInitializer? runtimeId with
          | some { id := foundId, stage := .pending token } =>
              if foundId = runtimeId ∧
                  s.findInitializing? key = some { runtimeId := runtimeId, key := key } ∧
                  s.findTopic? key = none ∧
                  (∀ topic ∈ s.byKey, topic.token ≠ token) ∧
                  tokenLive? runtime'.registry token = true then
                some { s with
                  runtime := runtime'
                  byKey := s.byKey ++ [{ key := key, token := token, stage := .provisional }] }
              else none
          | _ => none
      | none => none
  | .publishVisibleReuse key runtimeId slot generation =>
      match Runtime.apply? s.runtime (.insertPendingReuse runtimeId slot generation) with
      | some runtime' =>
          match runtime'.findInitializer? runtimeId with
          | some { id := foundId, stage := .pending token } =>
              if foundId = runtimeId ∧
                  s.findInitializing? key = some { runtimeId := runtimeId, key := key } ∧
                  s.findTopic? key = none ∧
                  (∀ topic ∈ s.byKey, topic.token ≠ token) ∧
                  tokenLive? runtime'.registry token = true then
                some { s with
                  runtime := runtime'
                  byKey := s.byKey ++ [{ key := key, token := token, stage := .provisional }] }
              else none
          | _ => none
      | none => none
  | .commitPublication key runtimeId =>
      match s.findTopic? key with
      | some topic =>
          if topic.stage = .provisional ∧ topic.key = key ∧
              s.findInitializing? key = some { runtimeId := runtimeId, key := key } ∧
              s.runtime.findInitializer? runtimeId =
                some { id := runtimeId, stage := .pending topic.token } then
            match Runtime.apply? s.runtime (.publishTopic runtimeId) with
            | some runtime' =>
                some { s with
                  runtime := runtime'
                  byKey := s.updateTopicStage key .committed }
            | none => none
          else none
      | none => none
  | .rollbackVisibleReuse key runtimeId nextGeneration =>
      match s.findTopic? key with
      | some topic =>
          if topic.stage = .provisional ∧ topic.key = key ∧
              s.findInitializing? key = some { runtimeId := runtimeId, key := key } ∧
              s.runtime.findInitializer? runtimeId =
                some { id := runtimeId, stage := .pending topic.token } then
            match Runtime.apply? s.runtime
                (.rollbackPendingReuse runtimeId nextGeneration) with
            | some runtime' =>
                some { s with
                  runtime := runtime'
                  byKey := s.removeTopic key }
            | none => none
          else none
      | none => none
  | .rollbackVisibleRetire key runtimeId =>
      match s.findTopic? key with
      | some topic =>
          if topic.stage = .provisional ∧ topic.key = key ∧
              s.findInitializing? key = some { runtimeId := runtimeId, key := key } ∧
              s.runtime.findInitializer? runtimeId =
                some { id := runtimeId, stage := .pending topic.token } then
            match Runtime.apply? s.runtime (.rollbackPendingRetire runtimeId) with
            | some runtime' =>
                some { s with
                  runtime := runtime'
                  byKey := s.removeTopic key }
            | none => none
          else none
      | none => none
  | .finishInitializer key runtimeId =>
      if s.findInitializing? key = some { runtimeId := runtimeId, key := key } ∧
          (∀ topic ∈ s.byKey, topic.key = key → topic.stage = .committed) then
        match Runtime.apply? s.runtime (.finishInitialize runtimeId) with
        | some runtime' =>
            some { s with
              runtime := runtime'
              initializing := s.removeInitializing runtimeId }
        | none => none
      else none

private theorem findTopic_stage_of_find
    {s : State} {key : TopicKey} {topic : Topic}
    (hFind : s.findTopic? key = some topic)
    (hStage : topic.stage = .provisional) :
    s.findTopic? key = some { topic with stage := .provisional } := by
  cases topic with
  | mk topicKey token stage =>
      dsimp at hStage ⊢
      cases hStage
      exact hFind

theorem apply?_sound
    {s s' : State} {event : Event}
    (h : apply? s event = some s') : Step s event s' := by
  cases event with
  | beginPrepare =>
      cases hRuntime : Runtime.apply? s.runtime .beginPrepare with
      | none => simp [apply?, hRuntime] at h
      | some runtime' =>
          simp only [apply?, hRuntime] at h
          cases h
          exact Step.beginPrepare (Runtime.apply?_sound hRuntime)
  | endPrepare =>
      cases hRuntime : Runtime.apply? s.runtime .endPrepare with
      | none => simp [apply?, hRuntime] at h
      | some runtime' =>
          simp only [apply?, hRuntime] at h
          cases h
          exact Step.endPrepare (Runtime.apply?_sound hRuntime)
  | sealTopics =>
      cases hRuntime : Runtime.apply? s.runtime .sealTopics with
      | none => simp [apply?, hRuntime] at h
      | some runtime' =>
          simp only [apply?, hRuntime] at h
          cases h
          exact Step.sealTopics (Runtime.apply?_sound hRuntime)
  | beginLookup token =>
      cases hRuntime : Runtime.apply? s.runtime (.beginLookup token) with
      | none => simp [apply?, hRuntime] at h
      | some runtime' =>
          simp only [apply?, hRuntime] at h
          cases h
          exact Step.beginLookup (Runtime.apply?_sound hRuntime)
  | endLookup =>
      cases hRuntime : Runtime.apply? s.runtime .endLookup with
      | none => simp [apply?, hRuntime] at h
      | some runtime' =>
          simp only [apply?, hRuntime] at h
          cases h
          exact Step.endLookup (Runtime.apply?_sound hRuntime)
  | beginInitializer key runtimeId =>
      dsimp [apply?] at h
      by_cases hPre : s.findTopic? key = none ∧
          s.findInitializing? key = none ∧
          (∀ init ∈ s.initializing, init.runtimeId ≠ runtimeId)
      · rw [if_pos hPre] at h
        cases hRuntime : Runtime.apply? s.runtime (.beginInitialize runtimeId) with
        | none => simp [hRuntime] at h
        | some runtime' =>
            rw [hRuntime] at h
            cases h
            exact Step.beginInitializer hPre.1 hPre.2.1 hPre.2.2
              (Runtime.apply?_sound hRuntime)
      · rw [if_neg hPre] at h
        contradiction
  | publishVisibleFresh key runtimeId =>
      cases hRuntime : Runtime.apply? s.runtime (.insertPendingFresh runtimeId) with
      | none => simp [apply?, hRuntime] at h
      | some runtime' =>
          simp only [apply?, hRuntime] at h
          split at h
          · rename_i _ foundId token hPending
            by_cases hPre : foundId = runtimeId ∧
                s.findInitializing? key = some { runtimeId := runtimeId, key := key } ∧
                s.findTopic? key = none ∧
                (∀ topic ∈ s.byKey, topic.token ≠ token) ∧
                tokenLive? runtime'.registry token = true
            · rw [if_pos hPre] at h
              cases h
              cases hPre.1
              exact Step.publishVisibleFresh hPre.2.1 hPre.2.2.1 hPre.2.2.2.1
                (Runtime.apply?_sound hRuntime) hPending
                (tokenLive?_iff.mp hPre.2.2.2.2)
            · rw [if_neg hPre] at h
              contradiction
          · contradiction
  | publishVisibleReuse key runtimeId slot generation =>
      cases hRuntime : Runtime.apply? s.runtime
          (.insertPendingReuse runtimeId slot generation) with
      | none => simp [apply?, hRuntime] at h
      | some runtime' =>
          simp only [apply?, hRuntime] at h
          split at h
          · rename_i _ foundId token hPending
            by_cases hPre : foundId = runtimeId ∧
                s.findInitializing? key = some { runtimeId := runtimeId, key := key } ∧
                s.findTopic? key = none ∧
                (∀ topic ∈ s.byKey, topic.token ≠ token) ∧
                tokenLive? runtime'.registry token = true
            · rw [if_pos hPre] at h
              cases h
              cases hPre.1
              exact Step.publishVisibleReuse hPre.2.1 hPre.2.2.1 hPre.2.2.2.1
                (Runtime.apply?_sound hRuntime) hPending
                (tokenLive?_iff.mp hPre.2.2.2.2)
            · rw [if_neg hPre] at h
              contradiction
          · contradiction
  | commitPublication key runtimeId =>
      dsimp [apply?] at h
      split at h
      · rename_i topic hTopicFind
        by_cases hPre : topic.stage = .provisional ∧ topic.key = key ∧
            s.findInitializing? key = some { runtimeId := runtimeId, key := key } ∧
            s.runtime.findInitializer? runtimeId =
              some { id := runtimeId, stage := .pending topic.token }
        · rw [if_pos hPre] at h
          cases hRuntime : Runtime.apply? s.runtime (.publishTopic runtimeId) with
          | none => simp [hRuntime] at h
          | some runtime' =>
              rw [hRuntime] at h
              cases h
              have hTopic : s.findTopic? key =
                  some { topic with stage := .provisional } := by
                exact findTopic_stage_of_find hTopicFind hPre.1
              exact Step.commitPublication hPre.2.2.1 hTopic hPre.2.1 hPre.2.2.2
                (Runtime.apply?_sound hRuntime)
        · rw [if_neg hPre] at h
          contradiction
      · contradiction
  | rollbackVisibleReuse key runtimeId nextGeneration =>
      dsimp [apply?] at h
      split at h
      · rename_i topic hTopicFind
        by_cases hPre : topic.stage = .provisional ∧ topic.key = key ∧
            s.findInitializing? key = some { runtimeId := runtimeId, key := key } ∧
            s.runtime.findInitializer? runtimeId =
              some { id := runtimeId, stage := .pending topic.token }
        · rw [if_pos hPre] at h
          cases hRuntime : Runtime.apply? s.runtime
              (.rollbackPendingReuse runtimeId nextGeneration) with
          | none => simp [hRuntime] at h
          | some runtime' =>
              rw [hRuntime] at h
              cases h
              have hTopic : s.findTopic? key =
                  some { topic with stage := .provisional } := by
                exact findTopic_stage_of_find hTopicFind hPre.1
              exact Step.rollbackVisibleReuse hPre.2.2.1 hTopic hPre.2.1 hPre.2.2.2
                (Runtime.apply?_sound hRuntime)
        · rw [if_neg hPre] at h
          contradiction
      · contradiction
  | rollbackVisibleRetire key runtimeId =>
      dsimp [apply?] at h
      split at h
      · rename_i topic hTopicFind
        by_cases hPre : topic.stage = .provisional ∧ topic.key = key ∧
            s.findInitializing? key = some { runtimeId := runtimeId, key := key } ∧
            s.runtime.findInitializer? runtimeId =
              some { id := runtimeId, stage := .pending topic.token }
        · rw [if_pos hPre] at h
          cases hRuntime : Runtime.apply? s.runtime (.rollbackPendingRetire runtimeId) with
          | none => simp [hRuntime] at h
          | some runtime' =>
              rw [hRuntime] at h
              cases h
              have hTopic : s.findTopic? key =
                  some { topic with stage := .provisional } := by
                exact findTopic_stage_of_find hTopicFind hPre.1
              exact Step.rollbackVisibleRetire hPre.2.2.1 hTopic hPre.2.1 hPre.2.2.2
                (Runtime.apply?_sound hRuntime)
        · rw [if_neg hPre] at h
          contradiction
      · contradiction
  | finishInitializer key runtimeId =>
      dsimp [apply?] at h
      by_cases hPre : s.findInitializing? key = some { runtimeId := runtimeId, key := key } ∧
          (∀ topic ∈ s.byKey, topic.key = key → topic.stage = .committed)
      · rw [if_pos hPre] at h
        cases hRuntime : Runtime.apply? s.runtime (.finishInitialize runtimeId) with
        | none => simp [hRuntime] at h
        | some runtime' =>
            rw [hRuntime] at h
            cases h
            exact Step.finishInitializer hPre.1 hPre.2 (Runtime.apply?_sound hRuntime)
      · rw [if_neg hPre] at h
        contradiction

theorem apply?_complete
    {s s' : State} {event : Event}
    (h : Step s event s') : apply? s event = some s' := by
  cases h with
  | beginPrepare hRuntime =>
      simp [apply?, Runtime.apply?_complete hRuntime]
  | endPrepare hRuntime =>
      simp [apply?, Runtime.apply?_complete hRuntime]
  | sealTopics hRuntime =>
      simp [apply?, Runtime.apply?_complete hRuntime]
  | beginLookup hRuntime =>
      simp [apply?, Runtime.apply?_complete hRuntime]
  | endLookup hRuntime =>
      simp [apply?, Runtime.apply?_complete hRuntime]
  | beginInitializer hNoTopic hNoInitializer hNoRuntimeId hRuntime =>
      dsimp [apply?]
      rw [if_pos ⟨hNoTopic, hNoInitializer, hNoRuntimeId⟩]
      rw [Runtime.apply?_complete hRuntime]
  | publishVisibleFresh hInit hNoTopic hNoToken hRuntime hPending hRoot =>
      have hRootBool := tokenLive?_iff.mpr hRoot
      dsimp [apply?]
      rw [Runtime.apply?_complete hRuntime]
      simp only
      rw [hPending]
      simp only
      have hPre : True ∧
          s.findInitializing? _ = some { runtimeId := _, key := _ } ∧
          s.findTopic? _ = none ∧
          (∀ topic ∈ s.byKey, topic.token ≠ _) ∧
          tokenLive? _ _ = true :=
        ⟨True.intro, hInit, hNoTopic, hNoToken, hRootBool⟩
      rw [if_pos hPre]
  | publishVisibleReuse hInit hNoTopic hNoToken hRuntime hPending hRoot =>
      have hRootBool := tokenLive?_iff.mpr hRoot
      dsimp [apply?]
      rw [Runtime.apply?_complete hRuntime]
      simp only
      rw [hPending]
      simp only
      have hPre : True ∧
          s.findInitializing? _ = some { runtimeId := _, key := _ } ∧
          s.findTopic? _ = none ∧
          (∀ topic ∈ s.byKey, topic.token ≠ _) ∧
          tokenLive? _ _ = true :=
        ⟨True.intro, hInit, hNoTopic, hNoToken, hRootBool⟩
      rw [if_pos hPre]
  | commitPublication hInit hTopic hTopicKey hPending hRuntime =>
      dsimp [apply?]
      rw [hTopic, Runtime.apply?_complete hRuntime]
      simp [hInit, hTopicKey, hPending]
  | rollbackVisibleReuse hInit hTopic hTopicKey hPending hRuntime =>
      dsimp [apply?]
      rw [hTopic, Runtime.apply?_complete hRuntime]
      simp [hInit, hTopicKey, hPending]
  | rollbackVisibleRetire hInit hTopic hTopicKey hPending hRuntime =>
      dsimp [apply?]
      rw [hTopic, Runtime.apply?_complete hRuntime]
      simp [hInit, hTopicKey, hPending]
  | finishInitializer hInit hReady hRuntime =>
      dsimp [apply?]
      rw [if_pos ⟨hInit, hReady⟩]
      rw [Runtime.apply?_complete hRuntime]

end XlFnFormal.Handle.Topics
