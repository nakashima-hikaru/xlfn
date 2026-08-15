import XlFnFormal.Handle.Topics.Invariant

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Refinement

open XlFnFormal.Handle.Topics

inductive PublicationState where
  | provisional
  | live
  | stale
  | closing
deriving DecidableEq, Repr

structure Publication where
  key : TopicKey
  token : Registry.Token
  rtdKey : RtdKey
  state : PublicationState
deriving DecidableEq, Repr

structure SnapshotBinding where
  key : TopicKey
  token : Registry.Token
deriving DecidableEq, Repr

structure WarmRead where
  id : Nat
  key : TopicKey
  token : Registry.Token
  rtdKey : RtdKey
deriving DecidableEq, Repr

structure State where
  topics : Topics.State
  publications : List Publication
  snapshot : List SnapshotBinding
  warmReads : List WarmRead
deriving DecidableEq, Repr

def initialState (topics : Topics.State) : State :=
  { topics := topics
    publications := []
    snapshot := []
    warmReads := [] }

def State.findPublication?
    (s : State) (key : TopicKey) (token : Registry.Token) : Option Publication :=
  s.publications.find? (fun publication =>
    publication.key == key && publication.token == token)

def State.findSnapshot?
    (s : State) (key : TopicKey) : Option SnapshotBinding :=
  s.snapshot.find? (fun binding => binding.key == key)

def State.findWarmRead?
    (s : State) (readerId : Nat) : Option WarmRead :=
  s.warmReads.find? (fun read => read.id == readerId)

def State.updatePublication
    (s : State) (key : TopicKey) (token : Registry.Token)
    (state : PublicationState) : List Publication :=
  s.publications.map (fun publication =>
    if publication.key == key && publication.token == token then
      { publication with state := state }
    else publication)

def State.removeSnapshot (s : State) (key : TopicKey) : List SnapshotBinding :=
  s.snapshot.filter (fun binding => binding.key != key)

def State.removeWarmRead (s : State) (readerId : Nat) : List WarmRead :=
  s.warmReads.filter (fun read => read.id != readerId)

def State.CanonicalTopicFor (s : State) (publication : Publication) : Prop :=
  ∃ topic ∈ s.topics.byKey,
    topic.key = publication.key ∧
    topic.token = publication.token ∧
    topic.rtdKey = publication.rtdKey ∧
    topic.stage = .committed

def State.CanonicalTopicForStage
    (s : State) (publication : Publication) (stage : Topics.TopicStage) : Prop :=
  ∃ topic ∈ s.topics.byKey,
    topic.key = publication.key ∧
    topic.token = publication.token ∧
    topic.rtdKey = publication.rtdKey ∧
    topic.stage = stage

def State.canonicalTopic? (s : State) (publication : Publication) : Bool :=
  s.topics.byKey.any (fun topic =>
    topic.key == publication.key &&
    topic.token == publication.token &&
    topic.rtdKey == publication.rtdKey &&
    topic.stage == .committed)

def State.PublicationIdentitiesUnique (s : State) : Prop :=
  s.publications.Pairwise (fun lhs rhs =>
    lhs.key ≠ rhs.key ∨ lhs.token ≠ rhs.token)

def State.SnapshotKeysUnique (s : State) : Prop :=
  s.snapshot.Pairwise (fun lhs rhs => lhs.key ≠ rhs.key)

def State.WarmReadersUnique (s : State) : Prop :=
  s.warmReads.Pairwise (fun lhs rhs => lhs.id ≠ rhs.id)

def State.LivePublicationSound (s : State) : Prop :=
  ∀ publication ∈ s.publications,
    publication.state = .live →
      s.CanonicalTopicFor publication

def State.ProvisionalPublicationSound (s : State) : Prop :=
  ∀ publication ∈ s.publications,
    publication.state = .provisional →
      s.CanonicalTopicForStage publication .provisional

def State.LiveSnapshotSound (s : State) : Prop :=
  ∀ binding ∈ s.snapshot,
    ∃ publication ∈ s.publications,
      publication.state = .live ∧
      publication.key = binding.key ∧
      publication.token = binding.token ∧
      s.CanonicalTopicFor publication

def State.LiveSnapshotHasCanonicalTopic (s : State) : Prop :=
  s.LiveSnapshotSound

def State.LiveSnapshotTokenMatchesCanonical (s : State) : Prop :=
  s.LiveSnapshotSound

def State.LiveSnapshotRtdKeyMatchesCanonical (s : State) : Prop :=
  s.LiveSnapshotSound

def State.LiveSnapshotRootIsLive (s : State) : Prop :=
  ∀ binding ∈ s.snapshot,
    ∃ publication ∈ s.publications,
      ∃ topic ∈ s.topics.byKey,
        publication.state = .live ∧
        publication.key = binding.key ∧
        publication.token = binding.token ∧
        topic.key = publication.key ∧
        topic.token = publication.token ∧
        topic.rtdKey = publication.rtdKey ∧
        topic.stage = .committed ∧
        Runtime.TokenLive s.topics.runtime.registry topic.token

def State.WarmReaderReferencesKnownPublication (s : State) : Prop :=
  ∀ read ∈ s.warmReads,
    ∃ publication ∈ s.publications,
      publication.key = read.key ∧
      publication.token = read.token ∧
      publication.rtdKey = read.rtdKey

def State.WarmReadsBound (s : State) : Prop :=
  s.warmReads.length ≤ s.topics.runtime.activePrepares

def State.WarmReadsBound? (s : State) : Bool :=
  s.warmReads.length ≤ s.topics.runtime.activePrepares

theorem warmReadsBound?_iff
    {s : State} :
    s.WarmReadsBound? = true ↔ s.WarmReadsBound := by
  simp [State.WarmReadsBound?, State.WarmReadsBound]

def State.Invariant (s : State) : Prop :=
  s.topics.Invariant ∧
  s.PublicationIdentitiesUnique ∧
  s.SnapshotKeysUnique ∧
  s.WarmReadersUnique ∧
  s.LivePublicationSound ∧
  s.ProvisionalPublicationSound ∧
  s.LiveSnapshotSound ∧
  s.LiveSnapshotRootIsLive ∧
  s.WarmReaderReferencesKnownPublication ∧
  s.WarmReadsBound

theorem initialInvariant
    {topics : Topics.State} (hTopics : topics.Invariant) :
    (initialState topics).Invariant := by
  refine ⟨hTopics, List.Pairwise.nil, List.Pairwise.nil, List.Pairwise.nil,
    ?_, ?_, ?_, ?_, ?_, ?_⟩
  · intro publication hMem hLive
    contradiction
  · intro publication hMem hProvisional
    contradiction
  · intro binding hMem
    contradiction
  · intro binding hMem
    contradiction
  · intro read hMem
    contradiction
  · simp [State.WarmReadsBound, initialState]

end XlFnFormal.Handle.Refinement
