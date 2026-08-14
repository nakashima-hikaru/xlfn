import XlFnFormal.Handle.Topics.Safety
import XlFnFormal.Handle.Runtime.Checker

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Topics

def tokenLive? (registry : Registry.State) (token : Registry.Token) : Bool :=
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
                  Registry.SlotState.live token.generation := hSlot.symm.trans hLive.choose_spec
              cases hEq
              rfl
  · simp [hSession]

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
      | some runtime' =>
          some { s with runtime := runtime', byKey := [], byRtdKey := [], byExcelOwner := [] }
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
  | .insertPendingFresh key runtimeId =>
      if s.findInitializing? key = some { runtimeId := runtimeId, key := key } ∧
          s.findTopic? key = none then
        match Runtime.apply? s.runtime (.insertPendingFresh runtimeId) with
        | some runtime' => some { s with runtime := runtime' }
        | none => none
      else none
  | .insertPendingReuse key runtimeId slot generation =>
      if s.findInitializing? key = some { runtimeId := runtimeId, key := key } ∧
          s.findTopic? key = none then
        match Runtime.apply? s.runtime
            (.insertPendingReuse runtimeId slot generation) with
        | some runtime' => some { s with runtime := runtime' }
        | none => none
      else none
  | .publishVisible key runtimeId rtdKey =>
      match s.runtime.findInitializer? runtimeId with
      | some { id := foundId, stage := .pending token } =>
          if foundId = runtimeId ∧
              s.runtime.phase = .open ∧
              s.findInitializing? key = some { runtimeId := runtimeId, key := key } ∧
              s.findTopic? key = none ∧
              s.findReverse? rtdKey = none ∧
              (∀ topic ∈ s.byKey, topic.token ≠ token) ∧
              tokenLive? s.runtime.registry token = true then
            some { s with
              byKey := s.byKey ++
                [{ key := key, rtdKey := rtdKey, token := token, stage := .provisional,
                   serverGeneration := none, excelOwner := none, excelCommitted := false }]
              byRtdKey := s.byRtdKey ++ [{ rtdKey := rtdKey, key := key }] }
          else none
      | _ => none
  | .claimServer key generation =>
      match s.findTopic? key with
      | some topic =>
          if topic.key = key ∧
              (topic.serverGeneration = none ∨
                topic.serverGeneration = some generation) then
            some { s with
              byKey := s.updateTopicServerGeneration key (some generation) }
          else none
      | none => none
  | .beginConnection key owner =>
      match s.findTopic? key with
      | some topic =>
          if topic.key = key ∧
              (topic.serverGeneration = none ∨
                topic.serverGeneration = some owner.serverGeneration) ∧
              topic.excelOwner = none ∧
              s.findExcelOwner? owner = none then
            some { s with
              byKey := s.updateTopicExcel key (some owner) false
              byExcelOwner := s.byExcelOwner ++ [{ owner := owner, key := key }] }
          else none
      | none => none
  | .reuseCommittedConnection key owner =>
      match s.findTopic? key with
      | some topic =>
          if topic.key = key ∧ topic.excelOwner = some owner ∧
              topic.serverGeneration = some owner.serverGeneration ∧
              topic.excelCommitted = true ∧
              s.findExcelOwner? owner = some { owner := owner, key := key } then
            some s
          else none
      | none => none
  | .commitConnection key owner =>
      match s.findTopic? key with
      | some topic =>
          if topic.key = key ∧ topic.excelOwner = some owner ∧
              topic.serverGeneration = some owner.serverGeneration ∧
              topic.excelCommitted = false ∧
              s.findExcelOwner? owner = some { owner := owner, key := key } then
            some { s with byKey := s.updateTopicExcel key (some owner) true }
          else none
      | none => none
  | .rollbackConnection key owner =>
      match s.findTopic? key with
      | some topic =>
          if topic.key = key ∧ topic.excelOwner = some owner ∧
              topic.serverGeneration = some owner.serverGeneration ∧
              topic.excelCommitted = false ∧
              s.findExcelOwner? owner = some { owner := owner, key := key } then
            some { s with
              byKey := s.updateTopicExcel key none false
              byExcelOwner := s.removeExcelOwner owner }
          else none
      | none => none
  | .commitPublication key runtimeId =>
      match s.findTopic? key with
      | some topic =>
          if topic.stage = .provisional ∧ topic.key = key ∧
              s.findInitializing? key = some { runtimeId := runtimeId, key := key } ∧
              (topic.excelOwner = none ∨ topic.excelCommitted = true) ∧
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
  | .withdrawVisible key runtimeId =>
      match s.findTopic? key with
      | some topic =>
          if topic.stage = .provisional ∧ topic.key = key ∧
              s.findInitializing? key = some { runtimeId := runtimeId, key := key } ∧
              (topic.excelOwner = none ∨ topic.excelCommitted = true) ∧
              s.runtime.findInitializer? runtimeId =
                some { id := runtimeId, stage := .pending topic.token } then
            some { s with
              byKey := s.removeTopic key
              byRtdKey := s.removeReverse topic.rtdKey
              byExcelOwner :=
                match topic.excelOwner with
                | some owner => s.removeExcelOwner owner
                | none => s.byExcelOwner }
          else none
      | none => none
  | .rollbackPendingReuse key runtimeId nextGeneration =>
      match s.runtime.findInitializer? runtimeId with
      | some { id := foundId, stage := .pending token } =>
          if foundId = runtimeId ∧
              s.findInitializing? key = some { runtimeId := runtimeId, key := key } ∧
              s.findTopic? key = none ∧
              (∀ topic ∈ s.byKey, topic.token ≠ token) then
            match Runtime.apply? s.runtime
                (.rollbackPendingReuse runtimeId nextGeneration) with
            | some runtime' => some { s with runtime := runtime' }
            | none => none
          else none
      | _ => none
  | .rollbackPendingRetire key runtimeId =>
      match s.runtime.findInitializer? runtimeId with
      | some { id := foundId, stage := .pending token } =>
          if foundId = runtimeId ∧
              s.findInitializing? key = some { runtimeId := runtimeId, key := key } ∧
              s.findTopic? key = none ∧
              (∀ topic ∈ s.byKey, topic.token ≠ token) then
            match Runtime.apply? s.runtime (.rollbackPendingRetire runtimeId) with
            | some runtime' => some { s with runtime := runtime' }
            | none => none
          else none
      | _ => none
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
  | .closeRegistry =>
      if s.byKey = [] ∧ s.byRtdKey = [] ∧ s.byExcelOwner = [] ∧
          s.initializing = [] ∧ s.detached = [] then
        match Runtime.apply? s.runtime .closeRegistry with
        | some runtime' => some { s with runtime := runtime' }
        | none => none
      else none
  | .finishClose =>
      match Runtime.apply? s.runtime .finishClose with
      | some runtime' => some { s with runtime := runtime' }
      | none => none

private theorem findTopic_stage_of_find
    {s : State} {key : TopicKey} {topic : Topic}
    (hFind : s.findTopic? key = some topic)
    (hStage : topic.stage = .provisional) :
    s.findTopic? key = some { topic with stage := .provisional } := by
  cases topic with
  | mk topicKey rtdKey token stage serverGeneration excelOwner excelCommitted =>
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
  | insertPendingFresh key runtimeId =>
      dsimp [apply?] at h
      by_cases hPre : s.findInitializing? key = some { runtimeId := runtimeId, key := key } ∧
          s.findTopic? key = none
      · rw [if_pos hPre] at h
        cases hRuntime : Runtime.apply? s.runtime (.insertPendingFresh runtimeId) with
        | none => simp [hRuntime] at h
        | some runtime' =>
            rw [hRuntime] at h
            cases h
            exact Step.insertPendingFresh hPre.1 hPre.2
              (Runtime.apply?_sound hRuntime)
      · rw [if_neg hPre] at h
        contradiction
  | insertPendingReuse key runtimeId slot generation =>
      dsimp [apply?] at h
      by_cases hPre : s.findInitializing? key = some { runtimeId := runtimeId, key := key } ∧
          s.findTopic? key = none
      · rw [if_pos hPre] at h
        cases hRuntime : Runtime.apply? s.runtime
            (.insertPendingReuse runtimeId slot generation) with
        | none => simp [hRuntime] at h
        | some runtime' =>
            rw [hRuntime] at h
            cases h
            exact Step.insertPendingReuse hPre.1 hPre.2
              (Runtime.apply?_sound hRuntime)
      · rw [if_neg hPre] at h
        contradiction
  | publishVisible key runtimeId rtdKey =>
      dsimp [apply?] at h
      cases hFind : s.runtime.findInitializer? runtimeId with
      | none => simp [hFind] at h
      | some found =>
          cases found with
          | mk foundId stage =>
              cases stage with
              | beforeInsert => simp [hFind] at h
              | resolved => simp [hFind] at h
              | pending token =>
                  simp only [hFind] at h
                  split at h
                  · rename_i hPre
                    cases h
                    rcases hPre with
                      ⟨hId, hPhase, hInit, hNoTopic, hNoRtdKey, hNoToken, hLive⟩
                    cases hId
                    exact Step.publishVisible hPhase hInit hNoTopic hNoRtdKey hNoToken hFind
                      (tokenLive?_iff.mp hLive)
                  · contradiction
  | claimServer key generation =>
      dsimp [apply?] at h
      cases hTopicFind : s.findTopic? key with
      | none => simp [hTopicFind] at h
      | some topic =>
          simp only [hTopicFind] at h
          split at h
          · rename_i hPre
            cases h
            rcases hPre with ⟨hTopicKey, hAllowed⟩
            exact Step.claimServer hTopicFind hTopicKey hAllowed
          · contradiction
  | beginConnection key owner =>
      dsimp [apply?] at h
      cases hTopicFind : s.findTopic? key with
      | none => simp [hTopicFind] at h
      | some topic =>
          simp only [hTopicFind] at h
          split at h
          · rename_i hPre
            cases h
            rcases hPre with ⟨hTopicKey, hGeneration, hTopicFree, hOwnerFree⟩
            exact Step.beginConnection hTopicFind hTopicKey hGeneration hTopicFree hOwnerFree
          · contradiction
  | reuseCommittedConnection key owner =>
      dsimp [apply?] at h
      cases hTopicFind : s.findTopic? key with
      | none => simp [hTopicFind] at h
      | some topic =>
          simp only [hTopicFind] at h
          split at h
          · rename_i hPre
            cases h
            rcases hPre with ⟨hTopicKey, hTopicOwner, hGeneration, hCommitted, hBinding⟩
            exact Step.reuseCommittedConnection hTopicFind hTopicKey hGeneration
              hTopicOwner hCommitted hBinding
          · contradiction
  | commitConnection key owner =>
      dsimp [apply?] at h
      cases hTopicFind : s.findTopic? key with
      | none => simp [hTopicFind] at h
      | some topic =>
          simp only [hTopicFind] at h
          split at h
          · rename_i hPre
            cases h
            rcases hPre with ⟨hTopicKey, hTopicOwner, hGeneration, hNotCommitted, hBinding⟩
            exact Step.commitConnection hTopicFind hTopicKey hGeneration hTopicOwner
              hNotCommitted hBinding
          · contradiction
  | rollbackConnection key owner =>
      dsimp [apply?] at h
      cases hTopicFind : s.findTopic? key with
      | none => simp [hTopicFind] at h
      | some topic =>
          simp only [hTopicFind] at h
          split at h
          · rename_i hPre
            cases h
            rcases hPre with ⟨hTopicKey, hTopicOwner, hGeneration, hNotCommitted, hBinding⟩
            exact Step.rollbackConnection hTopicFind hTopicKey hGeneration hTopicOwner
              hNotCommitted hBinding
          · contradiction
  | commitPublication key runtimeId =>
      dsimp [apply?] at h
      cases hTopicFind : s.findTopic? key with
      | none => simp [hTopicFind] at h
      | some topic =>
          simp only [hTopicFind] at h
          split at h
          · rename_i hPre
            cases hRuntime : Runtime.apply? s.runtime (.publishTopic runtimeId) with
            | none => simp [hRuntime] at h
            | some runtime' =>
                rw [hRuntime] at h
                cases h
                rcases hPre with ⟨hStage, hKey, hInit, hExcelSettled, hPending⟩
                have hTopic : s.findTopic? key =
                    some { topic with stage := .provisional } :=
                  findTopic_stage_of_find hTopicFind hStage
                exact Step.commitPublication hInit hTopic hKey hExcelSettled hPending
                  (Runtime.apply?_sound hRuntime)
          · contradiction
  | withdrawVisible key runtimeId =>
      dsimp [apply?] at h
      cases hTopicFind : s.findTopic? key with
      | none => simp [hTopicFind] at h
      | some topic =>
          simp only [hTopicFind] at h
          split at h
          · rename_i hPre
            cases h
            rcases hPre with ⟨hStage, hKey, hInit, hExcelSettled, hPending⟩
            have hTopic : s.findTopic? key =
                some { topic with stage := .provisional } :=
              findTopic_stage_of_find hTopicFind hStage
            exact Step.withdrawVisible hInit hTopic hKey hExcelSettled hPending
          · contradiction
  | rollbackPendingReuse key runtimeId nextGeneration =>
      dsimp [apply?] at h
      cases hFind : s.runtime.findInitializer? runtimeId with
      | none => simp [hFind] at h
      | some found =>
          cases found with
          | mk foundId stage =>
              cases stage with
              | beforeInsert => simp [hFind] at h
              | resolved => simp [hFind] at h
              | pending token =>
                  simp only [hFind] at h
                  split at h
                  · rename_i hPre
                    cases hRuntime : Runtime.apply? s.runtime
                        (.rollbackPendingReuse runtimeId nextGeneration) with
                    | none => simp [hRuntime] at h
                    | some runtime' =>
                        rw [hRuntime] at h
                        cases h
                        rcases hPre with ⟨hId, hInit, hNoTopic, hNoToken⟩
                        cases hId
                        exact Step.rollbackPendingReuse hInit hNoTopic hNoToken hFind
                          (Runtime.apply?_sound hRuntime)
                  · contradiction
  | rollbackPendingRetire key runtimeId =>
      dsimp [apply?] at h
      cases hFind : s.runtime.findInitializer? runtimeId with
      | none => simp [hFind] at h
      | some found =>
          cases found with
          | mk foundId stage =>
              cases stage with
              | beforeInsert => simp [hFind] at h
              | resolved => simp [hFind] at h
              | pending token =>
                  simp only [hFind] at h
                  split at h
                  · rename_i hPre
                    cases hRuntime : Runtime.apply? s.runtime (.rollbackPendingRetire runtimeId) with
                    | none => simp [hRuntime] at h
                    | some runtime' =>
                        rw [hRuntime] at h
                        cases h
                        rcases hPre with ⟨hId, hInit, hNoTopic, hNoToken⟩
                        cases hId
                        exact Step.rollbackPendingRetire hInit hNoTopic hNoToken hFind
                          (Runtime.apply?_sound hRuntime)
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
            exact Step.finishInitializer hPre.1 hPre.2
              (Runtime.apply?_sound hRuntime)
      · rw [if_neg hPre] at h
        contradiction
  | closeRegistry =>
      dsimp [apply?] at h
      by_cases hPre : s.byKey = [] ∧ s.byRtdKey = [] ∧
          s.byExcelOwner = [] ∧ s.initializing = [] ∧ s.detached = []
      · rw [if_pos hPre] at h
        cases hRuntime : Runtime.apply? s.runtime .closeRegistry with
        | none => simp [hRuntime] at h
        | some runtime' =>
            rw [hRuntime] at h
            cases h
            exact Step.closeRegistry hPre.1 hPre.2.1 hPre.2.2.1 hPre.2.2.2.1
              hPre.2.2.2.2
              (Runtime.apply?_sound hRuntime)
      · rw [if_neg hPre] at h
        contradiction
  | finishClose =>
      cases hRuntime : Runtime.apply? s.runtime .finishClose with
      | none => simp [apply?, hRuntime] at h
      | some runtime' =>
          simp only [apply?, hRuntime] at h
          cases h
          exact Step.finishClose (Runtime.apply?_sound hRuntime)

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
  | insertPendingFresh hInit hNoTopic hRuntime =>
      dsimp [apply?]
      rw [if_pos ⟨hInit, hNoTopic⟩]
      rw [Runtime.apply?_complete hRuntime]
  | insertPendingReuse hInit hNoTopic hRuntime =>
      dsimp [apply?]
      rw [if_pos ⟨hInit, hNoTopic⟩]
      rw [Runtime.apply?_complete hRuntime]
  | publishVisible hPhase hInit hNoTopic hNoRtdKey hNoToken hPending hRoot =>
      have hRootBool := tokenLive?_iff.mpr hRoot
      dsimp [apply?]
      rw [hPending]
      simp only
      have hPre : True ∧ s.runtime.phase = .open ∧
          s.findInitializing? _ = some { runtimeId := _, key := _ } ∧
          s.findTopic? _ = none ∧
          s.findReverse? _ = none ∧
          (∀ topic ∈ s.byKey, topic.token ≠ _) ∧
          tokenLive? _ _ = true :=
        ⟨True.intro, hPhase, hInit, hNoTopic, hNoRtdKey, hNoToken, hRootBool⟩
      rw [if_pos hPre]
  | claimServer hTopic hTopicKey hAllowed =>
      dsimp [apply?]
      simp only [hTopic]
      rw [if_pos ⟨hTopicKey, hAllowed⟩]
  | beginConnection hTopic hTopicKey hGeneration hTopicFree hOwnerFree =>
      dsimp [apply?]
      simp only [hTopic]
      rw [if_pos ⟨hTopicKey, hGeneration, hTopicFree, hOwnerFree⟩]
  | reuseCommittedConnection hTopic hTopicKey hGeneration hTopicOwner hCommitted hBinding =>
      dsimp [apply?]
      simp only [hTopic]
      rw [if_pos ⟨hTopicKey, hTopicOwner, hGeneration, hCommitted, hBinding⟩]
  | commitConnection hTopic hTopicKey hGeneration hTopicOwner hNotCommitted hBinding =>
      dsimp [apply?]
      simp only [hTopic]
      rw [if_pos ⟨hTopicKey, hTopicOwner, hGeneration, hNotCommitted, hBinding⟩]
  | rollbackConnection hTopic hTopicKey hGeneration hTopicOwner hNotCommitted hBinding =>
      dsimp [apply?]
      simp only [hTopic]
      rw [if_pos ⟨hTopicKey, hTopicOwner, hGeneration, hNotCommitted, hBinding⟩]
  | commitPublication hInit hTopic hTopicKey hExcelSettled hPending hRuntime =>
      dsimp [apply?]
      simp only [hTopic]
      simp only [Topic.ExcelConnectionSettled] at hExcelSettled
      have hPre : True ∧ _ = _ ∧ _ = _ ∧ _ ∧ _ = _ :=
        ⟨True.intro, hTopicKey, hInit, hExcelSettled, hPending⟩
      rw [if_pos hPre]
      rw [Runtime.apply?_complete hRuntime]
  | withdrawVisible hInit hTopic hTopicKey hExcelSettled hPending =>
      dsimp [apply?]
      simp only [hTopic]
      simp only [Topic.ExcelConnectionSettled] at hExcelSettled
      have hPre : True ∧ _ = _ ∧ _ = _ ∧ _ ∧ _ = _ :=
        ⟨True.intro, hTopicKey, hInit, hExcelSettled, hPending⟩
      rw [if_pos hPre]
      rfl
  | rollbackPendingReuse hInit hNoTopic hNoToken hPending hRuntime =>
      dsimp [apply?]
      rw [hPending]
      simp only
      rw [if_pos ⟨True.intro, hInit, hNoTopic, hNoToken⟩]
      rw [Runtime.apply?_complete hRuntime]
  | rollbackPendingRetire hInit hNoTopic hNoToken hPending hRuntime =>
      dsimp [apply?]
      rw [hPending]
      simp only
      rw [if_pos ⟨True.intro, hInit, hNoTopic, hNoToken⟩]
      rw [Runtime.apply?_complete hRuntime]
  | finishInitializer hInit hReady hRuntime =>
      dsimp [apply?]
      rw [if_pos ⟨hInit, hReady⟩]
      rw [Runtime.apply?_complete hRuntime]
  | closeRegistry hNoVisible hNoReverse hNoExcelOwners hNoInitializers hNoDetached hRuntime =>
      dsimp [apply?]
      rw [if_pos ⟨hNoVisible, hNoReverse, hNoExcelOwners, hNoInitializers, hNoDetached⟩]
      rw [Runtime.apply?_complete hRuntime]
  | finishClose hRuntime =>
      simp [apply?, Runtime.apply?_complete hRuntime]

end XlFnFormal.Handle.Topics
