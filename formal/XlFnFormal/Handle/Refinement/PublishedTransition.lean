import XlFnFormal.Handle.Refinement.PublishedModel
import XlFnFormal.Handle.Topics.Destruction

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Refinement

open XlFnFormal.Handle.Topics

inductive Event where
  | topic (event : Topics.Event)
  | installProvisional (key : TopicKey) (token : Registry.Token) (rtdKey : RtdKey)
  | commitAndActivate (key : TopicKey) (runtimeId : Runtime.InitializerId)
      (token : Registry.Token)
  | beginWarmRead (readerId : Nat) (key : TopicKey)
  | finishWarmRead (readerId : Nat)
  | failWarmRead (readerId : Nat)
  | abandonWarmRead (readerId : Nat)
  | disconnect (key : TopicKey) (owner : ExcelOwnerId)
  | detachGeneration (generation : ServerGeneration)
  | drainPendingReuse (token : Registry.Token) (runtimeId : Runtime.InitializerId)
      (nextGeneration : Registry.Generation)
  | drainPendingRetire (token : Registry.Token) (runtimeId : Runtime.InitializerId)
  | drainPublishedReuse (token : Registry.Token) (nextGeneration : Registry.Generation)
  | drainPublishedRetire (token : Registry.Token)
  | sealForClose
  | closeRegistry
deriving DecidableEq, Repr

def closingState : PublicationState → PublicationState
  | .provisional => .closing
  | .live => .closing
  | state => state

def topicLiftable? : Topics.Event → Bool
  | .commitPublication _ _ => false
  | .withdrawVisible _ _ => false
  | .sealTopics => false
  | .closeRegistry => false
  | _ => true

def State.updatePublicationState
    (s : State) (key : TopicKey) (token : Registry.Token)
    (state : PublicationState) : List Publication :=
  s.publications.map (fun publication =>
    if publication.key == key && publication.token == token then
      { publication with state := state }
    else publication)

def State.updateGenerationPublications
    (s : State) (generation : ServerGeneration) : List Publication :=
  s.publications.map (fun publication =>
    if s.topics.byKey.any (fun topic =>
        topic.key == publication.key &&
        topic.token == publication.token &&
        topic.serverGeneration == some generation) then
      { publication with state := .stale }
    else publication)

def State.removeSnapshotIdentity
    (s : State) (key : TopicKey) (token : Registry.Token) : List SnapshotBinding :=
  s.snapshot.filter (fun binding =>
    binding.key != key || binding.token != token)

def State.removeGenerationSnapshots
    (s : State) (generation : ServerGeneration) : List SnapshotBinding :=
  s.snapshot.filter (fun binding =>
    ¬s.topics.byKey.any (fun topic =>
      topic.key == binding.key &&
      topic.token == binding.token &&
      topic.serverGeneration == some generation))

def State.updateClosingPublications (s : State) : List Publication :=
  s.publications.map (fun publication =>
    { publication with state := closingState publication.state })

inductive Step : State → Event → State → Prop where
  | liftTopic
      {s : State} {topics' : Topics.State} {event : Topics.Event}
      (hLiftable : topicLiftable? event = true)
      (hTopics : Topics.Step s.topics event topics')
      (hBound : ({ s with topics := topics' }).WarmReadsBound) :
      Step s (.topic event) { s with topics := topics' }

  | installProvisional
      {s : State} {topic : Topic}
      {key : TopicKey} {token : Registry.Token} {rtdKey : RtdKey}
      (hTopic : s.topics.findTopic? key = some topic)
      (hTopicKey : topic.key = key)
      (hTopicToken : topic.token = token)
      (hTopicRtdKey : topic.rtdKey = rtdKey)
      (hStage : topic.stage = .provisional)
      (hNoPublication : s.findPublication? key token = none)
      (hNoSnapshot : s.findSnapshot? key = none) :
      Step s (.installProvisional key token rtdKey)
        { s with publications := s.publications ++
            [{ key := key, token := token, rtdKey := rtdKey, state := .provisional }] }

  | commitAndActivate
      {s : State} {topics' : Topics.State} {publication : Publication} {topic : Topic}
      {key : TopicKey} {runtimeId : Runtime.InitializerId} {token : Registry.Token}
      (hPublication : s.findPublication? key token = some publication)
      (hTopic : s.topics.findTopic? key = some topic)
      (hTopicKey : topic.key = key)
      (hTopicToken : topic.token = token)
      (hTopicRtdKey : topic.rtdKey = publication.rtdKey)
      (hStage : topic.stage = .provisional)
      (hPublicationState : publication.state = .provisional)
      (hNoSnapshot : s.findSnapshot? key = none)
      (hTopics : Topics.Step s.topics (.commitPublication key runtimeId) topics') :
      Step s (.commitAndActivate key runtimeId token)
        { s with
            topics := topics'
            publications := s.updatePublicationState key token .live
            snapshot := s.snapshot ++ [{ key := key, token := token }] }

  | beginWarmRead
      {s : State} {binding : SnapshotBinding} {publication : Publication}
      {readerId : Nat} {key : TopicKey}
      (hSnapshot : s.findSnapshot? key = some binding)
      (hPublication : s.findPublication? binding.key binding.token = some publication)
      (hLive : publication.state = .live)
      (hCanonical : s.canonicalTopic? publication = true)
      (hNoReader : s.findWarmRead? readerId = none)
      (hBound : s.warmReads.length < s.topics.runtime.activePrepares) :
      Step s (.beginWarmRead readerId key)
        { s with warmReads := s.warmReads ++
            [{ id := readerId, key := binding.key, token := binding.token,
               rtdKey := publication.rtdKey }] }

  | finishWarmRead
      {s : State} {read : WarmRead} {publication : Publication}
      {readerId : Nat}
      (hRead : s.findWarmRead? readerId = some read)
      (hPublication : s.findPublication? read.key read.token = some publication)
      (hLive : publication.state = .live)
      (hRtdKey : publication.rtdKey = read.rtdKey) :
      Step s (.finishWarmRead readerId)
        { s with warmReads := s.removeWarmRead readerId }

  | failWarmRead
      {s : State} {read : WarmRead} {publication : Publication}
      {readerId : Nat}
      (hRead : s.findWarmRead? readerId = some read)
      (hPublication : s.findPublication? read.key read.token = some publication)
      (hLive : publication.state = .live)
      (hRtdKey : publication.rtdKey = read.rtdKey) :
      Step s (.failWarmRead readerId)
        { s with warmReads := s.removeWarmRead readerId }

  | abandonWarmRead
      {s : State} {read : WarmRead} {publication : Publication}
      {readerId : Nat}
      (hRead : s.findWarmRead? readerId = some read)
      (hPublication : s.findPublication? read.key read.token = some publication)
      (hInvalidated : publication.state = .stale ∨ publication.state = .closing)
      (hRtdKey : publication.rtdKey = read.rtdKey) :
      Step s (.abandonWarmRead readerId)
        { s with warmReads := s.removeWarmRead readerId }

  | disconnect
      {s : State} {topics' : Topics.State} {topic : Topic} {publication : Publication}
      {key : TopicKey} {owner : ExcelOwnerId}
      (hTopic : s.topics.findTopic? key = some topic)
      (hTopicKey : topic.key = key)
      (hTopicOwner : topic.excelOwner = some owner)
      (hPublication : s.findPublication? key topic.token = some publication)
      (hDestroy : Topics.DestructionStep s.topics
        (.disconnectTopic key owner) topics') :
      Step s (.disconnect key owner)
        { s with
            topics := topics'
            publications := s.updatePublicationState key topic.token .stale
            snapshot := s.removeSnapshotIdentity key topic.token }

  | detachGeneration
      {s : State} {topics' : Topics.State} {generation : ServerGeneration}
      (hDestroy : Topics.DestructionStep s.topics
        (.detachGeneration generation) topics') :
      Step s (.detachGeneration generation)
        { s with
            topics := topics'
            publications := s.updateGenerationPublications generation
            snapshot := s.removeGenerationSnapshots generation }

  | drainPendingReuse
      {s : State} {topics' : Topics.State}
      {token : Registry.Token} {runtimeId : Runtime.InitializerId}
      {nextGeneration : Registry.Generation}
      (hDestroy : Topics.DestructionStep s.topics
        (.drainPendingReuse token runtimeId nextGeneration) topics') :
      Step s (.drainPendingReuse token runtimeId nextGeneration) { s with topics := topics' }

  | drainPendingRetire
      {s : State} {topics' : Topics.State}
      {token : Registry.Token} {runtimeId : Runtime.InitializerId}
      (hDestroy : Topics.DestructionStep s.topics
        (.drainPendingRetire token runtimeId) topics') :
      Step s (.drainPendingRetire token runtimeId) { s with topics := topics' }

  | drainPublishedReuse
      {s : State} {topics' : Topics.State}
      {token : Registry.Token} {nextGeneration : Registry.Generation}
      (hDestroy : Topics.DestructionStep s.topics
        (.drainPublishedReuse token nextGeneration) topics') :
      Step s (.drainPublishedReuse token nextGeneration) { s with topics := topics' }

  | drainPublishedRetire
      {s : State} {topics' : Topics.State} {token : Registry.Token}
      (hDestroy : Topics.DestructionStep s.topics
        (.drainPublishedRetire token) topics') :
      Step s (.drainPublishedRetire token) { s with topics := topics' }

  | sealForClose
      {s : State} {topics' : Topics.State}
      (hTopics : Topics.Step s.topics .sealTopics topics') :
      Step s .sealForClose
        { s with
            topics := topics'
            publications := s.updateClosingPublications
            snapshot := [] }

  | closeRegistry
      {s : State} {topics' : Topics.State}
      (hNoWarmReads : s.warmReads = [])
      (hNoSnapshot : s.snapshot = [])
      (hTopics : Topics.Step s.topics .closeRegistry topics') :
      Step s .closeRegistry { s with topics := topics' }

def apply? (s : State) (event : Event) : Option State :=
  match event with
  | .topic event =>
      if topicLiftable? event then
        match Topics.apply? s.topics event with
        | some topics' =>
            if hBound : ({ s with topics := topics' }).WarmReadsBound? = true then
              some { s with topics := topics' }
            else none
        | none => none
      else none
  | .installProvisional key token rtdKey =>
      match s.topics.findTopic? key with
      | some topic =>
          if topic.key = key ∧ topic.token = token ∧ topic.rtdKey = rtdKey ∧
              topic.stage = .provisional ∧
              s.findPublication? key token = none ∧
              s.findSnapshot? key = none then
            some { s with publications := s.publications ++
                [{ key := key, token := token, rtdKey := rtdKey, state := .provisional }] }
          else none
      | none => none
  | .commitAndActivate key runtimeId token =>
      match s.findPublication? key token, s.topics.findTopic? key with
      | some publication, some topic =>
          if topic.key = key ∧ topic.token = token ∧
              topic.rtdKey = publication.rtdKey ∧ topic.stage = .provisional ∧
              publication.state = .provisional ∧ s.findSnapshot? key = none then
            match Topics.apply? s.topics (.commitPublication key runtimeId) with
            | some topics' =>
                some { s with
                  topics := topics'
                  publications := s.updatePublicationState key token .live
                  snapshot := s.snapshot ++ [{ key := key, token := token }] }
            | none => none
          else none
      | _, _ => none
  | .beginWarmRead readerId key =>
      match s.findSnapshot? key with
      | some binding =>
          match s.findPublication? binding.key binding.token with
          | some publication =>
              if publication.state = .live ∧
                  s.canonicalTopic? publication = true ∧
                  s.findWarmRead? readerId = none ∧
                  s.warmReads.length < s.topics.runtime.activePrepares then
                some { s with warmReads := s.warmReads ++
                    [{ id := readerId, key := binding.key, token := binding.token,
                       rtdKey := publication.rtdKey }] }
              else none
          | none => none
      | none => none
  | .finishWarmRead readerId =>
      match s.findWarmRead? readerId with
      | some read =>
          match s.findPublication? read.key read.token with
          | some publication =>
              if publication.state = .live ∧ publication.rtdKey = read.rtdKey then
                some { s with warmReads := s.removeWarmRead readerId }
              else none
          | none => none
      | none => none
  | .failWarmRead readerId =>
      match s.findWarmRead? readerId with
      | some read =>
          match s.findPublication? read.key read.token with
          | some publication =>
              if publication.state = .live ∧ publication.rtdKey = read.rtdKey then
                some { s with warmReads := s.removeWarmRead readerId }
              else none
          | none => none
      | none => none
  | .abandonWarmRead readerId =>
      match s.findWarmRead? readerId with
      | some read =>
          match s.findPublication? read.key read.token with
          | some publication =>
              if (publication.state = .stale ∨ publication.state = .closing) ∧
                  publication.rtdKey = read.rtdKey then
                some { s with warmReads := s.removeWarmRead readerId }
              else none
          | none => none
      | none => none
  | .disconnect key owner =>
      match s.topics.findTopic? key with
      | some topic =>
          match s.findPublication? key topic.token with
          | some _ =>
              match Topics.applyDestruction? s.topics
                  (.disconnectTopic key owner) with
              | some topics' =>
                  some { s with
                    topics := topics'
                    publications := s.updatePublicationState key topic.token .stale
                    snapshot := s.removeSnapshotIdentity key topic.token }
              | none => none
          | none => none
      | none => none
  | .detachGeneration generation =>
      match Topics.applyDestruction? s.topics (.detachGeneration generation) with
      | some topics' =>
          some { s with
            topics := topics'
            publications := s.updateGenerationPublications generation
            snapshot := s.removeGenerationSnapshots generation }
      | none => none
  | .drainPendingReuse token runtimeId nextGeneration =>
      match Topics.applyDestruction? s.topics
          (.drainPendingReuse token runtimeId nextGeneration) with
      | some topics' => some { s with topics := topics' }
      | none => none
  | .drainPendingRetire token runtimeId =>
      match Topics.applyDestruction? s.topics
          (.drainPendingRetire token runtimeId) with
      | some topics' => some { s with topics := topics' }
      | none => none
  | .drainPublishedReuse token nextGeneration =>
      match Topics.applyDestruction? s.topics
          (.drainPublishedReuse token nextGeneration) with
      | some topics' => some { s with topics := topics' }
      | none => none
  | .drainPublishedRetire token =>
      match Topics.applyDestruction? s.topics (.drainPublishedRetire token) with
      | some topics' => some { s with topics := topics' }
      | none => none
  | .sealForClose =>
      match Topics.apply? s.topics .sealTopics with
      | some topics' =>
          some { s with
            topics := topics'
            publications := s.updateClosingPublications
            snapshot := [] }
      | none => none
  | .closeRegistry =>
      if s.warmReads = [] ∧ s.snapshot = [] then
        match Topics.apply? s.topics .closeRegistry with
        | some topics' => some { s with topics := topics' }
        | none => none
      else none

theorem apply?_sound
    {s s' : State} {event : Event}
    (h : apply? s event = some s') :
    Step s event s' := by
  cases event with
  | topic event =>
      dsimp [apply?] at h
      by_cases hLiftable : topicLiftable? event = true
      · rw [if_pos hLiftable] at h
        cases hTopics : Topics.apply? s.topics event with
        | none => simp [hTopics] at h
        | some topics' =>
            simp only [hTopics] at h
            by_cases hBound : ({ s with topics := topics' }).WarmReadsBound? = true
            · rw [if_pos hBound] at h
              cases h
              exact Step.liftTopic hLiftable
                (Topics.apply?_sound hTopics) (warmReadsBound?_iff.mp hBound)
            · rw [if_neg hBound] at h
              contradiction
      · rw [if_neg hLiftable] at h
        contradiction
  | installProvisional key token rtdKey =>
      dsimp [apply?] at h
      cases hTopic : s.topics.findTopic? key with
      | none => simp [hTopic] at h
      | some topic =>
          simp only [hTopic] at h
          by_cases hPre : topic.key = key ∧ topic.token = token ∧
              topic.rtdKey = rtdKey ∧ topic.stage = .provisional ∧
              s.findPublication? key token = none ∧ s.findSnapshot? key = none
          · rw [if_pos hPre] at h
            cases h
            exact Step.installProvisional hTopic hPre.1 hPre.2.1 hPre.2.2.1
              hPre.2.2.2.1 hPre.2.2.2.2.1 hPre.2.2.2.2.2
          · rw [if_neg hPre] at h
            contradiction
  | commitAndActivate key runtimeId token =>
      dsimp [apply?] at h
      cases hPub : s.findPublication? key token with
      | none => simp [hPub] at h
      | some publication =>
          simp only [hPub] at h
          cases hTopic : s.topics.findTopic? key with
          | none => simp [hTopic] at h
          | some topic =>
              simp only [hTopic] at h
              by_cases hPre : topic.key = key ∧ topic.token = token ∧
                  topic.rtdKey = publication.rtdKey ∧ topic.stage = .provisional ∧
                  publication.state = .provisional ∧ s.findSnapshot? key = none
              · rw [if_pos hPre] at h
                cases hTopics : Topics.apply? s.topics (.commitPublication key runtimeId) with
                | none => simp [hTopics] at h
                | some topics' =>
                    rw [hTopics] at h
                    cases h
                    exact Step.commitAndActivate hPub hTopic hPre.1 hPre.2.1
                      hPre.2.2.1 hPre.2.2.2.1 hPre.2.2.2.2.1
                      hPre.2.2.2.2.2
                      (Topics.apply?_sound hTopics)
              · rw [if_neg hPre] at h
                contradiction
  | beginWarmRead readerId key =>
      dsimp [apply?] at h
      cases hSnapshot : s.findSnapshot? key with
      | none => simp [hSnapshot] at h
      | some binding =>
          simp only [hSnapshot] at h
          cases hPub : s.findPublication? binding.key binding.token with
          | none => simp [hPub] at h
          | some publication =>
              simp only [hPub] at h
              by_cases hPre : publication.state = .live ∧
                  s.canonicalTopic? publication = true ∧
                  s.findWarmRead? readerId = none ∧
                  s.warmReads.length < s.topics.runtime.activePrepares
              · rw [if_pos hPre] at h
                cases h
                exact Step.beginWarmRead hSnapshot hPub hPre.1 hPre.2.1 hPre.2.2.1
                  hPre.2.2.2
              · rw [if_neg hPre] at h
                contradiction
  | finishWarmRead readerId =>
      dsimp [apply?] at h
      cases hRead : s.findWarmRead? readerId with
      | none => simp [hRead] at h
      | some read =>
          simp only [hRead] at h
          cases hPub : s.findPublication? read.key read.token with
          | none => simp [hPub] at h
          | some publication =>
              simp only [hPub] at h
              by_cases hPre : publication.state = .live ∧ publication.rtdKey = read.rtdKey
              · rw [if_pos hPre] at h
                cases h
                exact Step.finishWarmRead hRead hPub hPre.1 hPre.2
              · rw [if_neg hPre] at h
                contradiction
  | failWarmRead readerId =>
      dsimp [apply?] at h
      cases hRead : s.findWarmRead? readerId with
      | none => simp [hRead] at h
      | some read =>
          simp only [hRead] at h
          cases hPub : s.findPublication? read.key read.token with
          | none => simp [hPub] at h
          | some publication =>
              simp only [hPub] at h
              by_cases hPre : publication.state = .live ∧ publication.rtdKey = read.rtdKey
              · rw [if_pos hPre] at h
                cases h
                exact Step.failWarmRead hRead hPub hPre.1 hPre.2
              · rw [if_neg hPre] at h
                contradiction
  | abandonWarmRead readerId =>
      dsimp [apply?] at h
      cases hRead : s.findWarmRead? readerId with
      | none => simp [hRead] at h
      | some read =>
          simp only [hRead] at h
          cases hPub : s.findPublication? read.key read.token with
          | none => simp [hPub] at h
          | some publication =>
              simp only [hPub] at h
              by_cases hPre : (publication.state = .stale ∨
                  publication.state = .closing) ∧ publication.rtdKey = read.rtdKey
              · rw [if_pos hPre] at h
                cases h
                exact Step.abandonWarmRead hRead hPub hPre.1 hPre.2
              · rw [if_neg hPre] at h
                contradiction
  | disconnect key owner =>
      dsimp [apply?] at h
      cases hTopic : s.topics.findTopic? key with
      | none => simp [hTopic] at h
      | some topic =>
          simp only [hTopic] at h
          cases hPub : s.findPublication? key topic.token with
          | none => simp [hPub] at h
          | some publication =>
              simp only [hPub] at h
              cases hDestroy : Topics.applyDestruction? s.topics
                  (.disconnectTopic key owner) with
              | none => simp [hDestroy] at h
              | some topics' =>
                  rw [hDestroy] at h
                  cases h
                  have hDestroyStep := Topics.applyDestruction?_sound hDestroy
                  cases hDestroyStep with
                  | disconnectTopic hTopic' hTopicKey hTopicOwner hBinding hNoDetached =>
                      rename_i source
                      have hTopicEq : source = topic :=
                        Option.some.inj (hTopic'.symm.trans hTopic)
                      cases hTopicEq
                      exact Step.disconnect hTopic hTopicKey hTopicOwner hPub
                        (Topics.DestructionStep.disconnectTopic hTopic' hTopicKey
                          hTopicOwner hBinding hNoDetached)
  | detachGeneration generation =>
      dsimp [apply?] at h
      cases hDestroy : Topics.applyDestruction? s.topics
          (.detachGeneration generation) with
      | none => simp [hDestroy] at h
      | some topics' =>
          rw [hDestroy] at h
          cases h
          exact Step.detachGeneration (Topics.applyDestruction?_sound hDestroy)
  | drainPendingReuse token runtimeId nextGeneration =>
      dsimp [apply?] at h
      cases hDestroy : Topics.applyDestruction? s.topics
          (.drainPendingReuse token runtimeId nextGeneration) with
      | none => simp [hDestroy] at h
      | some topics' =>
          rw [hDestroy] at h
          cases h
          exact Step.drainPendingReuse (Topics.applyDestruction?_sound hDestroy)
  | drainPendingRetire token runtimeId =>
      dsimp [apply?] at h
      cases hDestroy : Topics.applyDestruction? s.topics
          (.drainPendingRetire token runtimeId) with
      | none => simp [hDestroy] at h
      | some topics' =>
          rw [hDestroy] at h
          cases h
          exact Step.drainPendingRetire (Topics.applyDestruction?_sound hDestroy)
  | drainPublishedReuse token nextGeneration =>
      dsimp [apply?] at h
      cases hDestroy : Topics.applyDestruction? s.topics
          (.drainPublishedReuse token nextGeneration) with
      | none => simp [hDestroy] at h
      | some topics' =>
          rw [hDestroy] at h
          cases h
          exact Step.drainPublishedReuse (Topics.applyDestruction?_sound hDestroy)
  | drainPublishedRetire token =>
      dsimp [apply?] at h
      cases hDestroy : Topics.applyDestruction? s.topics
          (.drainPublishedRetire token) with
      | none => simp [hDestroy] at h
      | some topics' =>
          rw [hDestroy] at h
          cases h
          exact Step.drainPublishedRetire (Topics.applyDestruction?_sound hDestroy)
  | sealForClose =>
      dsimp [apply?] at h
      cases hTopics : Topics.apply? s.topics .sealTopics with
      | none => simp [hTopics] at h
      | some topics' =>
          rw [hTopics] at h
          cases h
          exact Step.sealForClose (Topics.apply?_sound hTopics)
  | closeRegistry =>
      dsimp [apply?] at h
      by_cases hClose : s.warmReads = [] ∧ s.snapshot = []
      · rw [if_pos hClose] at h
        cases hTopics : Topics.apply? s.topics .closeRegistry with
        | none => simp [hTopics] at h
        | some topics' =>
            rw [hTopics] at h
            cases h
            exact Step.closeRegistry hClose.1 hClose.2
              (Topics.apply?_sound hTopics)
      · rw [if_neg hClose] at h
        contradiction

theorem apply?_complete
    {s s' : State} {event : Event}
    (h : Step s event s') :
    apply? s event = some s' := by
  cases h with
  | liftTopic hLiftable hTopics hBound =>
      simp [apply?, hLiftable, Topics.apply?_complete hTopics,
        warmReadsBound?_iff.mpr hBound]
  | installProvisional hTopic hTopicKey hTopicToken hTopicRtdKey hStage
      hNoPublication hNoSnapshot =>
      dsimp [apply?]
      simp only [hTopic]
      rw [if_pos ⟨hTopicKey, hTopicToken, hTopicRtdKey, hStage,
        hNoPublication, hNoSnapshot⟩]
  | commitAndActivate hPublication hTopic hTopicKey hTopicToken hTopicRtdKey
      hStage hPublicationState hNoSnapshot hTopics =>
      dsimp [apply?]
      simp only [hPublication, hTopic]
      rw [if_pos ⟨hTopicKey, hTopicToken, hTopicRtdKey, hStage,
        hPublicationState, hNoSnapshot⟩]
      rw [Topics.apply?_complete hTopics]
  | beginWarmRead hSnapshot hPublication hLive hCanonical hNoReader hBound =>
      dsimp [apply?]
      simp only [hSnapshot, hPublication]
      rw [if_pos ⟨hLive, hCanonical, hNoReader, hBound⟩]
  | finishWarmRead hRead hPublication hLive hRtdKey =>
      dsimp [apply?]
      simp only [hRead, hPublication]
      rw [if_pos ⟨hLive, hRtdKey⟩]
  | failWarmRead hRead hPublication hLive hRtdKey =>
      dsimp [apply?]
      simp only [hRead, hPublication]
      rw [if_pos ⟨hLive, hRtdKey⟩]
  | abandonWarmRead hRead hPublication hInvalidated hRtdKey =>
      dsimp [apply?]
      simp only [hRead, hPublication]
      rw [if_pos ⟨hInvalidated, hRtdKey⟩]
  | disconnect hTopic hTopicKey hTopicOwner hPublication hDestroy =>
      dsimp [apply?]
      simp only [hTopic, hPublication]
      rw [Topics.applyDestruction?_complete hDestroy]
  | detachGeneration hDestroy =>
      dsimp [apply?]
      rw [Topics.applyDestruction?_complete hDestroy]
  | drainPendingReuse hDestroy =>
      dsimp [apply?]
      rw [Topics.applyDestruction?_complete hDestroy]
  | drainPendingRetire hDestroy =>
      dsimp [apply?]
      rw [Topics.applyDestruction?_complete hDestroy]
  | drainPublishedReuse hDestroy =>
      dsimp [apply?]
      rw [Topics.applyDestruction?_complete hDestroy]
  | drainPublishedRetire hDestroy =>
      dsimp [apply?]
      rw [Topics.applyDestruction?_complete hDestroy]
  | sealForClose hTopics =>
      dsimp [apply?]
      rw [Topics.apply?_complete hTopics]
  | closeRegistry hNoWarmReads hNoSnapshot hTopics =>
      dsimp [apply?]
      rw [if_pos ⟨hNoWarmReads, hNoSnapshot⟩, Topics.apply?_complete hTopics]

end XlFnFormal.Handle.Refinement
