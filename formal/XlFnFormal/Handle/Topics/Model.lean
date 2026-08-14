import XlFnFormal.Handle.Runtime.Invariant

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Topics

structure TopicKey where
  sheetId : Nat
  row : Int
  column : Int
  udfId : String
  argumentDigest : Nat
deriving DecidableEq, Repr

abbrev RtdKey := String

structure ReverseTopic where
  rtdKey : RtdKey
  key : TopicKey
deriving DecidableEq, Repr

abbrev ServerGeneration := Nat
abbrev ExcelTopicId := Int

structure ExcelOwnerId where
  serverGeneration : ServerGeneration
  topicId : ExcelTopicId
deriving DecidableEq, Repr

structure ExcelBinding where
  owner : ExcelOwnerId
  key : TopicKey
deriving DecidableEq, Repr

inductive TopicStage where
  | provisional
  | committed
deriving DecidableEq, Repr

structure Initializer where
  runtimeId : Runtime.InitializerId
  key : TopicKey
deriving DecidableEq, Repr

structure Topic where
  key : TopicKey
  rtdKey : RtdKey
  token : Registry.Token
  stage : TopicStage
  serverGeneration : Option ServerGeneration
  excelOwner : Option ExcelOwnerId
  excelCommitted : Bool
deriving DecidableEq, Repr

def Topic.ExcelConnectionSettled (topic : Topic) : Prop :=
  topic.excelOwner = none ∨ topic.excelCommitted = true

structure DetachedTopic where
  topic : Topic
deriving DecidableEq, Repr

structure State where
  runtime : Runtime.State
  byKey : List Topic
  byRtdKey : List ReverseTopic
  byExcelOwner : List ExcelBinding
  initializing : List Initializer
  detached : List DetachedTopic
deriving DecidableEq, Repr

def initialState (session : Registry.SessionId) : State :=
  { runtime := Runtime.initialState session
    byKey := []
    byRtdKey := []
    byExcelOwner := []
    initializing := []
    detached := [] }

def State.findTopic? (s : State) (key : TopicKey) : Option Topic :=
  s.byKey.find? (fun topic => topic.key == key)

def State.findReverse? (s : State) (rtdKey : RtdKey) : Option ReverseTopic :=
  s.byRtdKey.find? (fun entry => entry.rtdKey == rtdKey)

def State.findExcelOwner? (s : State) (owner : ExcelOwnerId) : Option ExcelBinding :=
  s.byExcelOwner.find? (fun binding => binding.owner == owner)

def State.findInitializing? (s : State) (key : TopicKey) : Option Initializer :=
  s.initializing.find? (fun init => init.key == key)

def State.findDetached? (s : State) (token : Registry.Token) : Option DetachedTopic :=
  s.detached.find? (fun detached => detached.topic.token == token)

def State.findInitializerById? (s : State) (runtimeId : Runtime.InitializerId) : Option Initializer :=
  s.initializing.find? (fun init => init.runtimeId == runtimeId)

def State.removeInitializing (s : State) (runtimeId : Runtime.InitializerId) : List Initializer :=
  s.initializing.filter (fun init => init.runtimeId != runtimeId)

def State.removeTopic (s : State) (key : TopicKey) : List Topic :=
  s.byKey.filter (fun topic => topic.key != key)

def State.removeReverse (s : State) (rtdKey : RtdKey) : List ReverseTopic :=
  s.byRtdKey.filter (fun entry => entry.rtdKey != rtdKey)

def State.removeExcelOwner (s : State) (owner : ExcelOwnerId) : List ExcelBinding :=
  s.byExcelOwner.filter (fun binding => binding.owner != owner)

def State.removeDetached (s : State) (token : Registry.Token) : List DetachedTopic :=
  s.detached.filter (fun detached => detached.topic.token != token)

def State.updateTopicStage (s : State) (key : TopicKey) (stage : TopicStage) : List Topic :=
  s.byKey.map (fun topic => if topic.key == key then { topic with stage := stage } else topic)

def State.updateTopicExcel (s : State) (key : TopicKey)
    (owner : Option ExcelOwnerId) (committed : Bool) : List Topic :=
  s.byKey.map (fun topic =>
    if topic.key == key then
      { topic with
          serverGeneration :=
            match owner with
            | some owner => some owner.serverGeneration
            | none => topic.serverGeneration
          excelOwner := owner
          excelCommitted := committed }
    else topic)

def State.updateTopicServerGeneration (s : State) (key : TopicKey)
    (generation : Option ServerGeneration) : List Topic :=
  s.byKey.map (fun topic =>
    if topic.key == key then
      { topic with serverGeneration := generation }
    else topic)

def State.ReverseMapSound (s : State) : Prop :=
  ∀ entry ∈ s.byRtdKey,
    ∃ topic ∈ s.byKey,
      topic.key = entry.key ∧ topic.rtdKey = entry.rtdKey

def State.ReverseMapComplete (s : State) : Prop :=
  ∀ topic ∈ s.byKey,
    ∃ entry ∈ s.byRtdKey,
      entry.key = topic.key ∧ entry.rtdKey = topic.rtdKey

def State.RtdKeysUnique (s : State) : Prop :=
  s.byKey.Pairwise (fun lhs rhs => lhs.rtdKey ≠ rhs.rtdKey)

def State.ReverseRtdKeysUnique (s : State) : Prop :=
  s.byRtdKey.Pairwise (fun lhs rhs => lhs.rtdKey ≠ rhs.rtdKey)

def State.VisibleTopicRootsValid (s : State) : Prop :=
  ∀ topic ∈ s.byKey, Runtime.TokenLive s.runtime.registry topic.token

def State.InitializingKeysUnique (s : State) : Prop :=
  s.initializing.Pairwise (fun lhs rhs => lhs.key ≠ rhs.key)

def State.InitializerIdsUnique (s : State) : Prop :=
  s.initializing.Pairwise (fun lhs rhs => lhs.runtimeId ≠ rhs.runtimeId)

def State.InitializersBackedByRuntime (s : State) : Prop :=
  ∀ init ∈ s.initializing,
    ∃ runtimeInit ∈ s.runtime.initializers,
      runtimeInit.id = init.runtimeId

def State.VisibleKeysUnique (s : State) : Prop :=
  s.byKey.Pairwise (fun lhs rhs => lhs.key ≠ rhs.key)

def State.VisibleTokensUnique (s : State) : Prop :=
  s.byKey.Pairwise (fun lhs rhs => lhs.token ≠ rhs.token)

def State.ProvisionalTopicsHavePendingRoots (s : State) : Prop :=
  ∀ topic ∈ s.byKey,
    topic.stage = .provisional →
      ∃ init ∈ s.initializing,
        init.key = topic.key ∧
        s.runtime.findInitializer? init.runtimeId =
          some { id := init.runtimeId, stage := .pending topic.token }

def State.DetachedTokensUnique (s : State) : Prop :=
  s.detached.Pairwise (fun lhs rhs => lhs.topic.token ≠ rhs.topic.token)

def State.DetachedTokensDisjointVisible (s : State) : Prop :=
  ∀ detached ∈ s.detached,
    ∀ topic ∈ s.byKey,
      detached.topic.token ≠ topic.token

def State.DetachedRootsValid (s : State) : Prop :=
  ∀ detached ∈ s.detached,
    Runtime.TokenLive s.runtime.registry detached.topic.token

def State.DetachedProvisionalRootsHavePendingOwners (s : State) : Prop :=
  ∀ detached ∈ s.detached,
    detached.topic.stage = .provisional →
      ∃ init ∈ s.initializing,
        init.key = detached.topic.key ∧
        s.runtime.findInitializer? init.runtimeId =
          some { id := init.runtimeId, stage := .pending detached.topic.token }

def State.DestructionInvariant (s : State) : Prop :=
  s.DetachedTokensUnique ∧
  s.DetachedTokensDisjointVisible ∧
  s.DetachedRootsValid ∧
  s.DetachedProvisionalRootsHavePendingOwners

def State.CommittedTopicRootsValid (s : State) : Prop :=
  ∀ topic ∈ s.byKey,
    topic.stage = .committed →
      Runtime.TokenLive s.runtime.registry topic.token

def State.ExcelOwnerMapSound (s : State) : Prop :=
  ∀ binding ∈ s.byExcelOwner,
    ∃ topic ∈ s.byKey,
      topic.key = binding.key ∧
      topic.excelOwner = some binding.owner

def State.ExcelOwnerMapComplete (s : State) : Prop :=
  ∀ topic ∈ s.byKey,
    ∀ owner,
      topic.excelOwner = some owner →
        ∃ binding ∈ s.byExcelOwner,
          binding.owner = owner ∧ binding.key = topic.key

def State.ExcelOwnersUnique (s : State) : Prop :=
  ∀ owner lhs rhs,
    lhs ∈ s.byKey →
    rhs ∈ s.byKey →
    lhs.excelOwner = some owner →
    rhs.excelOwner = some owner →
    lhs.key = rhs.key

def State.ExcelBindingOwnersUnique (s : State) : Prop :=
  s.byExcelOwner.Pairwise (fun lhs rhs => lhs.owner ≠ rhs.owner)

def State.ExcelCommitConsistent (s : State) : Prop :=
  ∀ topic ∈ s.byKey,
    topic.excelCommitted = true →
      ∃ owner, topic.excelOwner = some owner

def State.ExcelOwnerGenerationConsistent (s : State) : Prop :=
  ∀ topic ∈ s.byKey,
    ∀ owner,
      topic.excelOwner = some owner →
        topic.serverGeneration = some owner.serverGeneration

def State.ExcelOwnershipInvariant (s : State) : Prop :=
  s.ExcelOwnerMapSound ∧
  s.ExcelOwnerMapComplete ∧
  s.ExcelOwnersUnique ∧
  s.ExcelBindingOwnersUnique ∧
  s.ExcelCommitConsistent

def State.Invariant (s : State) : Prop :=
  Runtime.RuntimeInvariant s.runtime ∧
  s.InitializingKeysUnique ∧
  s.InitializerIdsUnique ∧
  s.InitializersBackedByRuntime ∧
  s.VisibleKeysUnique ∧
  s.VisibleTokensUnique ∧
  s.RtdKeysUnique ∧
  s.ReverseRtdKeysUnique ∧
  s.ReverseMapSound ∧
  s.ReverseMapComplete ∧
  s.VisibleTopicRootsValid ∧
  s.ProvisionalTopicsHavePendingRoots ∧
  s.ExcelOwnershipInvariant ∧
  s.ExcelOwnerGenerationConsistent ∧
  s.DestructionInvariant

end XlFnFormal.Handle.Topics
