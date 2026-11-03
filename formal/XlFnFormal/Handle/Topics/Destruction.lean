import XlFnFormal.Handle.Topics.Checker

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Topics

inductive DestructionEvent where
  | disconnectTopic (key : TopicKey) (owner : ExcelOwnerId)
  | detachGeneration (generation : ServerGeneration)
  | drainPendingReuse (token : Registry.Token) (runtimeId : Runtime.InitializerId)
      (nextGeneration : Registry.Generation)
  | drainPendingRetire (token : Registry.Token) (runtimeId : Runtime.InitializerId)
  | drainPublishedReuse (token : Registry.Token) (nextGeneration : Registry.Generation)
  | drainPublishedRetire (token : Registry.Token)
deriving DecidableEq, Repr

inductive DestructionStep : State → DestructionEvent → State → Prop where
  | disconnectTopic
      {s : State} {topic : Topic}
      {key : TopicKey} {owner : ExcelOwnerId}
      (hTopic : s.findTopic? key = some topic)
      (hTopicKey : topic.key = key)
      (hTopicOwner : topic.excelOwner = some owner)
      (hBinding : s.findExcelOwner? owner = some { owner := owner, key := key })
      (hNoDetached : s.findDetached? topic.token = none) :
      DestructionStep s (.disconnectTopic key owner)
        { s with
            byKey := s.removeTopic key
            byRtdKey := s.removeReverse topic.rtdKey
            byExcelOwner := s.removeExcelOwner owner
            detached := s.detached ++ [{ topic := topic }] }

  | detachGeneration
      {s : State} {generation : ServerGeneration} :
      DestructionStep s (.detachGeneration generation)
        { s with
            byKey := s.removeGenerationTopics generation
            byRtdKey := s.removeGenerationReverse generation
            byExcelOwner := s.removeGenerationExcelOwners generation
            detached := s.detached ++ s.detachedGeneration generation }

  | drainPendingReuse
      {s : State} {sRuntime : Runtime.State} {detached : DetachedTopic}
      {token : Registry.Token} {runtimeId : Runtime.InitializerId}
      {nextGeneration : Registry.Generation}
      (hDetached : s.findDetached? token = some detached)
      (hInit : s.findInitializing? detached.topic.key =
        some { runtimeId := runtimeId, key := detached.topic.key })
      (hPending : s.runtime.findInitializer? runtimeId =
        some { id := runtimeId, stage := .pending token })
      (hRuntime : Runtime.Step s.runtime
        (.rollbackPendingReuse runtimeId nextGeneration) sRuntime) :
      DestructionStep s (.drainPendingReuse token runtimeId nextGeneration)
        { s with
            runtime := sRuntime
            detached := s.removeDetached token }

  | drainPendingRetire
      {s : State} {sRuntime : Runtime.State} {detached : DetachedTopic}
      {token : Registry.Token} {runtimeId : Runtime.InitializerId}
      (hDetached : s.findDetached? token = some detached)
      (hInit : s.findInitializing? detached.topic.key =
        some { runtimeId := runtimeId, key := detached.topic.key })
      (hPending : s.runtime.findInitializer? runtimeId =
        some { id := runtimeId, stage := .pending token })
      (hRuntime : Runtime.Step s.runtime
        (.rollbackPendingRetire runtimeId) sRuntime) :
      DestructionStep s (.drainPendingRetire token runtimeId)
        { s with
            runtime := sRuntime
            detached := s.removeDetached token }

  | drainPublishedReuse
      {s : State} {registry' : Registry.State} {detached : DetachedTopic}
      {token : Registry.Token} {nextGeneration : Registry.Generation}
      (hDetached : s.findDetached? token = some detached)
      (hPublished : detached.topic.stage = .committed)
      (hNoPending : ∀ init ∈ s.runtime.initializers,
        init.stage ≠ .pending token)
      (hRegistry : Registry.Step s.runtime.registry
        (.removeReuse token nextGeneration) registry') :
      DestructionStep s (.drainPublishedReuse token nextGeneration)
        { s with
            runtime := { s.runtime with registry := registry' }
            detached := s.removeDetached token }

  | drainPublishedRetire
      {s : State} {registry' : Registry.State} {detached : DetachedTopic}
      {token : Registry.Token}
      (hDetached : s.findDetached? token = some detached)
      (hPublished : detached.topic.stage = .committed)
      (hNoPending : ∀ init ∈ s.runtime.initializers,
        init.stage ≠ .pending token)
      (hRegistry : Registry.Step s.runtime.registry
        (.removeRetire token) registry') :
      DestructionStep s (.drainPublishedRetire token)
        { s with
            runtime := { s.runtime with registry := registry' }
            detached := s.removeDetached token }

def applyDestruction? (s : State) (e : DestructionEvent) : Option State :=
  match e with
  | .disconnectTopic key owner =>
      match s.findTopic? key with
      | some topic =>
          if topic.key = key ∧
              topic.excelOwner = some owner ∧
              s.findExcelOwner? owner = some { owner := owner, key := key } ∧
              s.findDetached? topic.token = none then
            some { s with
              byKey := s.removeTopic key
              byRtdKey := s.removeReverse topic.rtdKey
              byExcelOwner := s.removeExcelOwner owner
              detached := s.detached ++ [{ topic := topic }] }
          else none
      | none => none
  | .detachGeneration generation =>
      some { s with
        byKey := s.removeGenerationTopics generation
        byRtdKey := s.removeGenerationReverse generation
        byExcelOwner := s.removeGenerationExcelOwners generation
        detached := s.detached ++ s.detachedGeneration generation }
  | .drainPendingReuse token runtimeId nextGeneration =>
      match s.findDetached? token with
      | some detached =>
          if s.findInitializing? detached.topic.key =
                some { runtimeId := runtimeId, key := detached.topic.key } ∧
              s.runtime.findInitializer? runtimeId =
                some { id := runtimeId, stage := .pending token } then
            match Runtime.apply? s.runtime
                (.rollbackPendingReuse runtimeId nextGeneration) with
            | some runtime' =>
                some { s with
                  runtime := runtime'
                  detached := s.removeDetached token }
            | none => none
          else none
      | none => none
  | .drainPendingRetire token runtimeId =>
      match s.findDetached? token with
      | some detached =>
          if s.findInitializing? detached.topic.key =
                some { runtimeId := runtimeId, key := detached.topic.key } ∧
              s.runtime.findInitializer? runtimeId =
                some { id := runtimeId, stage := .pending token } then
            match Runtime.apply? s.runtime (.rollbackPendingRetire runtimeId) with
            | some runtime' =>
                some { s with
                  runtime := runtime'
                  detached := s.removeDetached token }
            | none => none
          else none
      | none => none
  | .drainPublishedReuse token nextGeneration =>
      match s.findDetached? token with
      | some detached =>
          if detached.topic.stage = .committed ∧
              (∀ init ∈ s.runtime.initializers, init.stage ≠ .pending token) then
            match Registry.apply? s.runtime.registry
                (.removeReuse token nextGeneration) with
            | some registry' =>
                some { s with
                  runtime := { s.runtime with registry := registry' }
                  detached := s.removeDetached token }
            | none => none
          else none
      | none => none
  | .drainPublishedRetire token =>
      match s.findDetached? token with
      | some detached =>
          if detached.topic.stage = .committed ∧
              (∀ init ∈ s.runtime.initializers, init.stage ≠ .pending token) then
            match Registry.apply? s.runtime.registry (.removeRetire token) with
            | some registry' =>
                some { s with
                  runtime := { s.runtime with registry := registry' }
                  detached := s.removeDetached token }
            | none => none
          else none
      | none => none

theorem applyDestruction?_sound
    {s s' : State} {event : DestructionEvent}
    (h : applyDestruction? s event = some s') :
    DestructionStep s event s' := by
  cases event with
  | disconnectTopic key owner =>
      dsimp [applyDestruction?] at h
      cases hTopic : s.findTopic? key with
      | none => simp [hTopic] at h
      | some topic =>
          simp only [hTopic] at h
          by_cases hPre : topic.key = key ∧
              topic.excelOwner = some owner ∧
              s.findExcelOwner? owner = some { owner := owner, key := key } ∧
              s.findDetached? topic.token = none
          · rw [if_pos hPre] at h
            cases h
            exact DestructionStep.disconnectTopic hTopic hPre.1 hPre.2.1
              hPre.2.2.1 hPre.2.2.2
          · rw [if_neg hPre] at h
            contradiction
  | detachGeneration generation =>
      dsimp [applyDestruction?] at h
      cases h
      exact DestructionStep.detachGeneration
  | drainPendingReuse token runtimeId nextGeneration =>
      dsimp [applyDestruction?] at h
      cases hDetached : s.findDetached? token with
      | none => simp [hDetached] at h
      | some detached =>
          simp only [hDetached] at h
          by_cases hPre :
              s.findInitializing? detached.topic.key =
                  some { runtimeId := runtimeId, key := detached.topic.key } ∧
              s.runtime.findInitializer? runtimeId =
                  some { id := runtimeId, stage := .pending token }
          · rw [if_pos hPre] at h
            cases hRuntime : Runtime.apply? s.runtime
                (.rollbackPendingReuse runtimeId nextGeneration) with
            | none => simp [hRuntime] at h
            | some runtime' =>
                rw [hRuntime] at h
                cases h
                exact DestructionStep.drainPendingReuse hDetached hPre.1 hPre.2
                  (Runtime.apply?_sound hRuntime)
          · rw [if_neg hPre] at h
            contradiction
  | drainPendingRetire token runtimeId =>
      dsimp [applyDestruction?] at h
      cases hDetached : s.findDetached? token with
      | none => simp [hDetached] at h
      | some detached =>
          simp only [hDetached] at h
          by_cases hPre :
              s.findInitializing? detached.topic.key =
                  some { runtimeId := runtimeId, key := detached.topic.key } ∧
              s.runtime.findInitializer? runtimeId =
                  some { id := runtimeId, stage := .pending token }
          · rw [if_pos hPre] at h
            cases hRuntime : Runtime.apply? s.runtime
                (.rollbackPendingRetire runtimeId) with
            | none => simp [hRuntime] at h
            | some runtime' =>
                rw [hRuntime] at h
                cases h
                exact DestructionStep.drainPendingRetire hDetached hPre.1 hPre.2
                  (Runtime.apply?_sound hRuntime)
          · rw [if_neg hPre] at h
            contradiction
  | drainPublishedReuse token nextGeneration =>
      dsimp [applyDestruction?] at h
      cases hDetached : s.findDetached? token with
      | none => simp [hDetached] at h
      | some detached =>
          simp only [hDetached] at h
          by_cases hPre : detached.topic.stage = .committed ∧
              (∀ init ∈ s.runtime.initializers, init.stage ≠ .pending token)
          · rw [if_pos hPre] at h
            cases hRegistry : Registry.apply? s.runtime.registry
                (.removeReuse token nextGeneration) with
            | none => simp [hRegistry] at h
            | some registry' =>
                rw [hRegistry] at h
                cases h
                exact DestructionStep.drainPublishedReuse hDetached hPre.1 hPre.2
                  (Registry.apply?_sound hRegistry)
          · rw [if_neg hPre] at h
            contradiction
  | drainPublishedRetire token =>
      dsimp [applyDestruction?] at h
      cases hDetached : s.findDetached? token with
      | none => simp [hDetached] at h
      | some detached =>
          simp only [hDetached] at h
          by_cases hPre : detached.topic.stage = .committed ∧
              (∀ init ∈ s.runtime.initializers, init.stage ≠ .pending token)
          · rw [if_pos hPre] at h
            cases hRegistry : Registry.apply? s.runtime.registry
                (.removeRetire token) with
            | none => simp [hRegistry] at h
            | some registry' =>
                rw [hRegistry] at h
                cases h
                exact DestructionStep.drainPublishedRetire hDetached hPre.1 hPre.2
                  (Registry.apply?_sound hRegistry)
          · rw [if_neg hPre] at h
            contradiction

theorem applyDestruction?_complete
    {s s' : State} {event : DestructionEvent}
    (h : DestructionStep s event s') :
    applyDestruction? s event = some s' := by
  cases h with
  | disconnectTopic hTopic hTopicKey hTopicOwner hBinding hNoDetached =>
      dsimp [applyDestruction?]
      simp only [hTopic]
      rw [if_pos ⟨hTopicKey, hTopicOwner, hBinding, hNoDetached⟩]
  | detachGeneration =>
      rfl
  | drainPendingReuse hDetached hInit hPending hRuntime =>
      dsimp [applyDestruction?]
      simp only [hDetached]
      rw [if_pos ⟨hInit, hPending⟩]
      rw [Runtime.apply?_complete hRuntime]
  | drainPendingRetire hDetached hInit hPending hRuntime =>
      dsimp [applyDestruction?]
      simp only [hDetached]
      rw [if_pos ⟨hInit, hPending⟩]
      rw [Runtime.apply?_complete hRuntime]
  | drainPublishedReuse hDetached hPublished hNoPending hRegistry =>
      dsimp [applyDestruction?]
      simp only [hDetached]
      rw [if_pos ⟨hPublished, hNoPending⟩]
      rw [Registry.apply?_complete hRegistry]
  | drainPublishedRetire hDetached hPublished hNoPending hRegistry =>
      dsimp [applyDestruction?]
      simp only [hDetached]
      rw [if_pos ⟨hPublished, hNoPending⟩]
      rw [Registry.apply?_complete hRegistry]

end XlFnFormal.Handle.Topics
