import XlFnFormal.Handle.Topics.Invariant
import XlFnFormal.Handle.Runtime.Safety

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
    have hRelation := pairwise_mem_ne_topics hInv.2.1 hLeft hRight hEq
    cases hRelation with
    | inl hNotEqual => exact hNotEqual (hLeftKey.trans hRightKey.symm)
    | inr hNotEqual => exact hNotEqual (hRightKey.trans hLeftKey.symm)

theorem same_key_has_at_most_one_visible_topic
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
    have hRelation := pairwise_mem_ne_topics hInv.2.2.2.2.1 hLeft hRight hEq
    cases hRelation with
    | inl hNotEqual => exact hNotEqual (hLeftKey.trans hRightKey.symm)
    | inr hNotEqual => exact hNotEqual (hRightKey.trans hLeftKey.symm)

theorem same_key_has_at_most_one_committed_topic
    {s : State} {key : TopicKey} {left right : Topic}
    (hInv : s.Invariant)
    (hLeft : left ∈ s.byKey)
    (hRight : right ∈ s.byKey)
    (hLeftStage : left.stage = .committed)
    (hRightStage : right.stage = .committed)
    (hLeftKey : left.key = key)
    (hRightKey : right.key = key) :
    left = right :=
  same_key_has_at_most_one_visible_topic hInv hLeft hRight hLeftKey hRightKey

theorem distinct_visible_topics_have_distinct_tokens
    {s : State} {left right : Topic}
    (hInv : s.Invariant)
    (hLeft : left ∈ s.byKey)
    (hRight : right ∈ s.byKey)
    (hNe : left ≠ right) :
    left.token ≠ right.token := by
  have hRelation := pairwise_mem_ne_topics hInv.2.2.2.2.2.1 hLeft hRight hNe
  cases hRelation with
  | inl hNotEqual => exact hNotEqual
  | inr hNotEqual => exact hNotEqual.symm

theorem distinct_committed_topics_have_distinct_tokens
    {s : State} {left right : Topic}
    (hInv : s.Invariant)
    (hLeft : left ∈ s.byKey)
    (hRight : right ∈ s.byKey)
    (hLeftStage : left.stage = .committed)
    (hRightStage : right.stage = .committed)
    (hNe : left ≠ right) :
    left.token ≠ right.token :=
  distinct_visible_topics_have_distinct_tokens hInv hLeft hRight hNe

theorem committed_topic_root_is_live
    {s : State} {topic : Topic}
  (hInv : s.Invariant)
  (hTopic : topic ∈ s.byKey)
  (hCommitted : topic.stage = .committed) :
    Runtime.TokenLive s.runtime.registry topic.token :=
  hInv.2.2.2.2.2.2.2.2.2.2.1 topic hTopic

theorem provisional_topic_has_pending_provenance
    {s : State} {topic : Topic}
    (hInv : s.Invariant)
    (hTopic : topic ∈ s.byKey)
    (hProvisional : topic.stage = .provisional) :
    ∃ init ∈ s.initializing,
      init.key = topic.key ∧
      s.runtime.findInitializer? init.runtimeId =
        some { id := init.runtimeId, stage := .pending topic.token } :=
  by
    rcases hInv with ⟨_, _, _, _, _, _, _, _, _, _, _, hProv⟩
    exact hProv.1 topic hTopic hProvisional

theorem reverse_lookup_resolves_visible_topic
    {s : State} {rtdKey : RtdKey} {entry : ReverseTopic}
    (hInv : s.Invariant)
    (hFind : s.findReverse? rtdKey = some entry) :
    entry.rtdKey = rtdKey ∧
      ∃ topic ∈ s.byKey,
        topic.key = entry.key ∧ topic.rtdKey = rtdKey := by
  have hSound : s.ReverseMapSound := hInv.2.2.2.2.2.2.2.2.1
  have hRtdKey := rtdKey_of_findReverse_some hFind
  refine ⟨hRtdKey, ?_⟩
  rcases hSound entry (mem_of_findReverse_some hFind) with
    ⟨topic, hTopic, hTopicKey, hTopicRtdKey⟩
  exact ⟨topic, hTopic, hTopicKey, hTopicRtdKey.trans hRtdKey⟩

theorem visible_topic_has_reverse_lookup
    {s : State} {topic : Topic}
    (hInv : s.Invariant)
    (hTopic : topic ∈ s.byKey) :
    ∃ entry,
      s.findReverse? topic.rtdKey = some entry ∧
      entry.key = topic.key := by
  have hComplete : s.ReverseMapComplete := hInv.2.2.2.2.2.2.2.2.2.1
  rcases hComplete topic hTopic with ⟨entry, hEntryMem, hEntryKey, hEntryRtdKey⟩
  have hSome : (s.findReverse? topic.rtdKey).isSome = true := by
    dsimp [State.findReverse?]
    rw [List.find?_isSome]
    exact ⟨entry, hEntryMem, beq_iff_eq.mpr hEntryRtdKey⟩
  generalize hFind : s.findReverse? topic.rtdKey = output at hSome
  cases output with
  | none => simp at hSome
  | some found =>
      have hSound : s.ReverseMapSound := hInv.2.2.2.2.2.2.2.2.1
      rcases hSound found (mem_of_findReverse_some hFind) with
        ⟨visible, hVisible, hVisibleKey, hVisibleRtdKey⟩
      have hFoundRtdKey := rtdKey_of_findReverse_some hFind
      have hVisibleEq : visible = topic := by
        by_cases hEq : visible = topic
        · exact hEq
        · exfalso
          have hDistinct := distinct_topics_have_rtd_keys
            hInv.2.2.2.2.2.2.1 hVisible hTopic hEq
          exact hDistinct (hVisibleRtdKey.trans hFoundRtdKey)
      refine ⟨found, rfl, ?_⟩
      exact hVisibleKey.symm.trans (congrArg Topic.key hVisibleEq)

theorem publish_visible_exposes_pending_provenance
    {s s' : State} {key : TopicKey} {runtimeId : Runtime.InitializerId}
    {rtdKey : RtdKey}
    (hStep : Step s (.publishVisible key runtimeId rtdKey) s') :
    ∃ topic,
      topic ∈ s'.byKey ∧
      topic.key = key ∧
      topic.rtdKey = rtdKey ∧
      topic.stage = .provisional ∧
      s'.runtime.findInitializer? runtimeId =
        some { id := runtimeId, stage := .pending topic.token } := by
  cases hStep with
  | publishVisible hPhase hInit hNoTopic hNoRtdKey hNoToken hPending hRoot =>
      rename_i token
      refine ⟨Topic.mk key rtdKey token .provisional none false,
        ?_, rfl, rfl, rfl, ?_⟩
      · simp
      · exact hPending

theorem commit_publication_resolves_initializer
    {s s' : State} {key : TopicKey} {runtimeId : Runtime.InitializerId}
    (hStep : Step s (.commitPublication key runtimeId) s') :
    ∃ topic,
      topic ∈ s'.byKey ∧
      topic.key = key ∧
      topic.stage = .committed ∧
      s'.runtime.findInitializer? runtimeId =
        some { id := runtimeId, stage := .resolved } := by
  cases hStep with
  | commitPublication hInit hTopic hTopicKey hPending hRuntime =>
      rename_i source
      have hTopicMem : { source with stage := .provisional } ∈ s.byKey :=
        mem_of_findTopic_some hTopic
      refine ⟨Topic.mk key source.rtdKey source.token .committed
        source.excelOwner source.excelCommitted,
        ?_, rfl, rfl, ?_⟩
      · dsimp [State.updateTopicStage]
        simp only [List.mem_map]
        refine ⟨{ source with stage := .provisional }, hTopicMem, ?_⟩
        simp [hTopicKey]
      · cases hRuntime
        dsimp [Runtime.State.findInitializer?]
        have hResolved := Runtime.updateInitializer_find hPending (stage := .resolved)
        exact hResolved

theorem withdraw_visible_removes_topic_key
    {s s' : State} {key : TopicKey} {runtimeId : Runtime.InitializerId}
    (hStep : Step s (.withdrawVisible key runtimeId) s') :
    ∀ topic ∈ s'.byKey, topic.key ≠ key := by
  cases hStep with
  | withdrawVisible hInit hTopic hTopicKey hPending =>
      intro topic hMem
      dsimp [State.removeTopic] at hMem
      rcases List.mem_filter.mp hMem with ⟨_, hNe⟩
      intro hEq
      have hFalse : (topic.key != key) = false := by simp [hEq]
      rw [hFalse] at hNe
      contradiction

theorem no_topic_publication_after_seal
    {s : State} {key : TopicKey} {runtimeId : Runtime.InitializerId} {rtdKey : RtdKey}
    (hSealed : s.runtime.phase = .drainingPrepares) :
    ¬ ∃ s', Step s (.publishVisible key runtimeId rtdKey) s' := by
  intro ⟨s', hStep⟩
  cases hStep with
  | publishVisible hPhase =>
      rw [hSealed] at hPhase
      contradiction

theorem Reachable.runtime_reachable
    {s t : State} (hReach : Reachable s t) :
    Runtime.Reachable s.runtime t.runtime := by
  induction hReach with
  | refl => exact Runtime.Reachable.refl _
  | tail _ hStep ih =>
      cases hStep with
      | beginPrepare hRuntime => exact Runtime.Reachable.tail ih hRuntime
      | endPrepare hRuntime => exact Runtime.Reachable.tail ih hRuntime
      | sealTopics hRuntime => exact Runtime.Reachable.tail ih hRuntime
      | beginLookup hRuntime => exact Runtime.Reachable.tail ih hRuntime
      | endLookup hRuntime => exact Runtime.Reachable.tail ih hRuntime
      | beginInitializer _ _ _ hRuntime => exact Runtime.Reachable.tail ih hRuntime
      | insertPendingFresh _ _ hRuntime => exact Runtime.Reachable.tail ih hRuntime
      | insertPendingReuse _ _ hRuntime => exact Runtime.Reachable.tail ih hRuntime
      | publishVisible => exact ih
      | beginConnection => exact ih
      | reuseCommittedConnection => exact ih
      | commitConnection => exact ih
      | rollbackConnection => exact ih
      | commitPublication _ _ _ _ hRuntime => exact Runtime.Reachable.tail ih hRuntime
      | withdrawVisible => exact ih
      | rollbackPendingReuse _ _ _ _ hRuntime => exact Runtime.Reachable.tail ih hRuntime
      | rollbackPendingRetire _ _ _ _ hRuntime => exact Runtime.Reachable.tail ih hRuntime
      | finishInitializer _ _ hRuntime => exact Runtime.Reachable.tail ih hRuntime
      | closeRegistry _ _ _ _ hRuntime => exact Runtime.Reachable.tail ih hRuntime
      | finishClose hRuntime => exact Runtime.Reachable.tail ih hRuntime

def CloseCertified (s : State) : Prop :=
  Runtime.CloseCertified s.runtime ∧
  s.byKey = [] ∧
  s.byRtdKey = [] ∧
  s.byExcelOwner = [] ∧
  s.initializing = []

theorem no_visible_topics_when_closed
    {s : State} (hInv : s.Invariant)
    (hClosed : s.runtime.phase = .closed) :
    s.byKey = [] := by
  have hPhase := Runtime.phaseInvariant_closed_fields hInv.1.1 hClosed
  cases hByKey : s.byKey with
  | nil => rfl
  | cons head tail =>
      exfalso
      have hMem : head ∈ s.byKey := by
        rw [hByKey]
        exact List.mem_cons_self
      rcases hInv.2.2.2.2.2.2.2.2.2.2.1 head hMem with
        ⟨hSession, ⟨hBounds, hSlot⟩⟩
      have hNoLive := hPhase.2.2.2.2 head.token.slot hBounds
      apply hNoLive
      rw [hSlot]
      trivial

theorem no_reverse_entries_when_closed
    {s : State} (hInv : s.Invariant)
    (hClosed : s.runtime.phase = .closed) :
    s.byRtdKey = [] := by
  have hSound : s.ReverseMapSound := hInv.2.2.2.2.2.2.2.2.1
  cases hReverse : s.byRtdKey with
  | nil => rfl
  | cons head tail =>
      exfalso
      have hMem : head ∈ s.byRtdKey := by
        rw [hReverse]
        exact List.mem_cons_self
      rcases hSound head hMem with ⟨topic, hTopic, _, _⟩
      have hNoVisible := no_visible_topics_when_closed hInv hClosed
      rw [hNoVisible] at hTopic
      contradiction

theorem no_excel_owners_when_closed
    {s : State} (hInv : s.Invariant)
    (hClosed : s.runtime.phase = .closed) :
    s.byExcelOwner = [] := by
  have hNoVisible := no_visible_topics_when_closed hInv hClosed
  rcases hInv with
    ⟨_, _, _, _, _, _, _, _, _, _, _, hProvAndExcel⟩
  have hSound : s.ExcelOwnerMapSound := hProvAndExcel.2.1
  cases hOwners : s.byExcelOwner with
  | nil => rfl
  | cons head tail =>
      exfalso
      have hMem : head ∈ s.byExcelOwner := by
        rw [hOwners]
        exact List.mem_cons_self
      rcases hSound head hMem with ⟨topic, hTopic, _, _⟩
      rw [hNoVisible] at hTopic
      contradiction

theorem no_initializers_when_runtime_empty
    {s : State} (hInv : s.Invariant)
    (hRuntimeEmpty : s.runtime.initializers = []) :
    s.initializing = [] := by
  cases hInitializers : s.initializing with
  | nil => rfl
  | cons head tail =>
      exfalso
      have hMem : head ∈ s.initializing := by
        rw [hInitializers]
        exact List.mem_cons_self
      rcases hInv.2.2.2.1 head hMem with ⟨runtimeInit, hRuntimeMem, hId⟩
      rw [hRuntimeEmpty] at hRuntimeMem
      contradiction

theorem successful_close_is_certified
    {session : Registry.SessionId} {s : State}
    (hReach : Reachable (initialState session) s)
    (hClosed : s.runtime.phase = .closed) :
    CloseCertified s := by
  have hInv := reachable_invariant session hReach
  have hRuntimeReach := Reachable.runtime_reachable hReach
  have hRuntimeCert := Runtime.successful_close_is_certified hRuntimeReach hClosed
  exact ⟨hRuntimeCert,
    no_visible_topics_when_closed hInv hClosed,
    no_reverse_entries_when_closed hInv hClosed,
    no_excel_owners_when_closed hInv hClosed,
    no_initializers_when_runtime_empty hInv hRuntimeCert.2.2.1⟩

theorem Step.closeCertified_of_finishClose
    {s s' : State}
    (hReach : Reachable (initialState s.runtime.registry.session) s)
    (hStep : Step s .finishClose s') :
    CloseCertified s' := by
  cases hStep with
  | finishClose hRuntime =>
      cases hRuntime with
      | finishClose hPhase hRegStep =>
          exact successful_close_is_certified
            (Reachable.tail hReach (Step.finishClose
              (Runtime.Step.finishClose hPhase hRegStep))) rfl

theorem close_registry_waits_for_topic_quiescence
    {s s' : State} (hStep : Step s .closeRegistry s') :
    s.byKey = [] ∧ s.byRtdKey = [] ∧ s.byExcelOwner = [] ∧ s.initializing = [] := by
  cases hStep
  exact ⟨by assumption, by assumption, by assumption, by assumption⟩

end XlFnFormal.Handle.Topics
