import XlFnFormal.Handle.Topics.Model

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Topics

inductive Event where
  | beginInitialize (key : TopicKey) (owner : OwnerId)
  | publish (key : TopicKey) (owner : OwnerId) (rtdKey : RtdKey) (token : Registry.Token)
  | abortInitialize (key : TopicKey) (owner : OwnerId)
deriving DecidableEq, Repr

inductive Step : State → Event → State → Prop where
  | beginInitialize
      {s : State}
      {key : TopicKey}
      {owner : OwnerId}
      (hNotClosed : s.registry.closed = false)
      (hNoTopic : s.findTopic? key = none)
      (hNoInitializer : s.findInitializing? key = none) :
      Step s (.beginInitialize key owner)
        { s with initializing := s.initializing ++ [{ key := key, owner := owner }] }

  | publish
      {s : State}
      {key : TopicKey}
      {owner : OwnerId}
      {rtdKey : RtdKey}
      {token : Registry.Token}
      (hFind : s.findInitializing? key = some { key := key, owner := owner })
      (hNoTopic : s.findTopic? key = none)
      (hRoot : Registry.TokenLive s.registry token) :
      Step s (.publish key owner rtdKey token)
        { s with
            byKey := s.byKey ++ [{ key := key, rtdKey := rtdKey, token := token }]
            byRtdKey := s.byRtdKey ++ [(rtdKey, key)]
            initializing := s.removeInitializing key }

  | abortInitialize
      {s : State}
      {key : TopicKey}
      {owner : OwnerId}
      (hFind : s.findInitializing? key = some { key := key, owner := owner }) :
      Step s (.abortInitialize key owner)
        { s with initializing := s.removeInitializing key }

inductive Reachable : State → State → Prop where
  | refl (s : State) : Reachable s s
  | tail {s t u : State} {e : Event} : Reachable s t → Step t e u → Reachable s u

end XlFnFormal.Handle.Topics
