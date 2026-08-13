import XlFnFormal.Handle.Registry.Model

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Topics

open Registry (SessionId Token)

abbrev OwnerId := Nat
abbrev RtdKey := String

structure TopicKey where
  sheetId : Nat
  row : Int
  column : Int
  udfId : String
  argumentDigest : Nat
deriving DecidableEq, Repr

def TopicKey.formatRtdKey (key : TopicKey) : RtdKey :=
  let separator := String.singleton (Char.ofNat 31)
  s!"{key.sheetId}{separator}{key.row}{separator}{key.column}{separator}{key.udfId}{separator}{key.argumentDigest}"

structure Initializer where
  key : TopicKey
  owner : OwnerId
deriving DecidableEq, Repr

structure Topic where
  key : TopicKey
  rtdKey : RtdKey
  token : Token
deriving DecidableEq, Repr

structure State where
  registry : Registry.State
  byKey : List Topic
  byRtdKey : List (RtdKey × TopicKey)
  initializing : List Initializer
deriving DecidableEq, Repr

def initialState (registry : Registry.State) : State :=
  { registry := registry
    byKey := []
    byRtdKey := []
    initializing := [] }

def State.findTopic? (s : State) (key : TopicKey) : Option Topic :=
  s.byKey.find? (fun topic => topic.key == key)

def State.findInitializing? (s : State) (key : TopicKey) : Option Initializer :=
  s.initializing.find? (fun init => init.key == key)

def State.removeInitializing (s : State) (key : TopicKey) : List Initializer :=
  s.initializing.filter (fun init => init.key != key)

def State.CommittedTopicRootValid (s : State) (topic : Topic) : Prop :=
  Registry.TokenLive s.registry topic.token

def State.InitializingKeysUnique (s : State) : Prop :=
  s.initializing.Pairwise (fun lhs rhs => lhs.key ≠ rhs.key)

def State.CommittedKeysUnique (s : State) : Prop :=
  s.byKey.Pairwise (fun lhs rhs => lhs.key ≠ rhs.key)

def State.CommittedTopicRootsValid (s : State) : Prop :=
  ∀ topic ∈ s.byKey, s.CommittedTopicRootValid topic

def State.Invariant (s : State) : Prop :=
  s.InitializingKeysUnique ∧
  s.CommittedKeysUnique ∧
  s.CommittedTopicRootsValid

end XlFnFormal.Handle.Topics
