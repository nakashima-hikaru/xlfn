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
deriving DecidableEq, Repr

structure State where
  runtime : Runtime.State
  byKey : List Topic
  byRtdKey : List ReverseTopic
  initializing : List Initializer
deriving DecidableEq, Repr

def initialState (session : Registry.SessionId) : State :=
  { runtime := Runtime.initialState session
    byKey := []
    byRtdKey := []
    initializing := [] }

def State.findTopic? (s : State) (key : TopicKey) : Option Topic :=
  s.byKey.find? (fun topic => topic.key == key)

def State.findReverse? (s : State) (rtdKey : RtdKey) : Option ReverseTopic :=
  s.byRtdKey.find? (fun entry => entry.rtdKey == rtdKey)

def State.findInitializing? (s : State) (key : TopicKey) : Option Initializer :=
  s.initializing.find? (fun init => init.key == key)

def State.findInitializerById? (s : State) (runtimeId : Runtime.InitializerId) : Option Initializer :=
  s.initializing.find? (fun init => init.runtimeId == runtimeId)

def State.removeInitializing (s : State) (runtimeId : Runtime.InitializerId) : List Initializer :=
  s.initializing.filter (fun init => init.runtimeId != runtimeId)

def State.removeTopic (s : State) (key : TopicKey) : List Topic :=
  s.byKey.filter (fun topic => topic.key != key)

def State.removeReverse (s : State) (rtdKey : RtdKey) : List ReverseTopic :=
  s.byRtdKey.filter (fun entry => entry.rtdKey != rtdKey)

def State.updateTopicStage (s : State) (key : TopicKey) (stage : TopicStage) : List Topic :=
  s.byKey.map (fun topic => if topic.key == key then { topic with stage := stage } else topic)

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

def State.CommittedTopicRootsValid (s : State) : Prop :=
  ∀ topic ∈ s.byKey,
    topic.stage = .committed →
      Runtime.TokenLive s.runtime.registry topic.token

def State.Invariant (s : State) : Prop :=
  Runtime.RuntimeInvariant s.runtime ∧
  s.InitializingKeysUnique ∧
  s.InitializerIdsUnique ∧
  s.InitializersBackedByRuntime ∧
  s.VisibleKeysUnique ∧
  s.VisibleTokensUnique ∧
  s.RtdKeysUnique ∧
  s.ReverseMapSound ∧
  s.ReverseMapComplete ∧
  s.VisibleTopicRootsValid ∧
  s.ProvisionalTopicsHavePendingRoots

end XlFnFormal.Handle.Topics
