import XlFnFormal.Handle.Topics.Model
import XlFnFormal.Handle.Runtime.Transition

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Topics

inductive Event where
  | beginPrepare
  | endPrepare
  | sealTopics
  | beginLookup (token : Registry.Token)
  | endLookup
  | beginInitializer (key : TopicKey) (runtimeId : Runtime.InitializerId)
  | publishVisibleFresh (key : TopicKey) (runtimeId : Runtime.InitializerId)
  | publishVisibleReuse (key : TopicKey) (runtimeId : Runtime.InitializerId)
      (slot : Registry.SlotId) (generation : Registry.Generation)
  | commitPublication (key : TopicKey) (runtimeId : Runtime.InitializerId)
  | rollbackVisibleReuse (key : TopicKey) (runtimeId : Runtime.InitializerId)
      (nextGeneration : Registry.Generation)
  | rollbackVisibleRetire (key : TopicKey) (runtimeId : Runtime.InitializerId)
  | finishInitializer (key : TopicKey) (runtimeId : Runtime.InitializerId)
deriving DecidableEq, Repr

inductive Step : State → Event → State → Prop where
  | beginPrepare
      {s : State} {runtime' : Runtime.State}
      (hRuntime : Runtime.Step s.runtime .beginPrepare runtime') :
      Step s .beginPrepare { s with runtime := runtime' }

  | endPrepare
      {s : State} {runtime' : Runtime.State}
      (hRuntime : Runtime.Step s.runtime .endPrepare runtime') :
      Step s .endPrepare { s with runtime := runtime' }

  | sealTopics
      {s : State} {runtime' : Runtime.State}
      (hRuntime : Runtime.Step s.runtime .sealTopics runtime') :
      Step s .sealTopics { s with runtime := runtime' }

  | beginLookup
      {s : State} {runtime' : Runtime.State} {token : Registry.Token}
      (hRuntime : Runtime.Step s.runtime (.beginLookup token) runtime') :
      Step s (.beginLookup token) { s with runtime := runtime' }

  | endLookup
      {s : State} {runtime' : Runtime.State}
      (hRuntime : Runtime.Step s.runtime .endLookup runtime') :
      Step s .endLookup { s with runtime := runtime' }

  | beginInitializer
      {s : State} {runtime' : Runtime.State}
      {key : TopicKey} {runtimeId : Runtime.InitializerId}
      (hNoTopic : s.findTopic? key = none)
      (hNoInitializer : s.findInitializing? key = none)
      (hNoRuntimeId : ∀ init ∈ s.initializing, init.runtimeId ≠ runtimeId)
      (hRuntime : Runtime.Step s.runtime (.beginInitialize runtimeId) runtime') :
      Step s (.beginInitializer key runtimeId)
        { s with
            runtime := runtime'
            initializing := s.initializing ++ [{ runtimeId := runtimeId, key := key }] }

  | publishVisibleFresh
      {s : State} {runtime' : Runtime.State} {token : Registry.Token}
      {key : TopicKey} {runtimeId : Runtime.InitializerId}
      (hInit : s.findInitializing? key = some { runtimeId := runtimeId, key := key })
      (hNoTopic : s.findTopic? key = none)
      (hNoToken : ∀ topic ∈ s.byKey, topic.token ≠ token)
      (hRuntime : Runtime.Step s.runtime (.insertPendingFresh runtimeId) runtime')
      (hPending : runtime'.findInitializer? runtimeId =
        some { id := runtimeId, stage := .pending token })
      (hRoot : Runtime.TokenLive runtime'.registry token) :
      Step s (.publishVisibleFresh key runtimeId)
        { s with
            runtime := runtime'
            byKey := s.byKey ++ [{ key := key, token := token, stage := .provisional }] }

  | publishVisibleReuse
      {s : State} {runtime' : Runtime.State} {token : Registry.Token}
      {key : TopicKey} {runtimeId : Runtime.InitializerId}
      {slot : Registry.SlotId} {generation : Registry.Generation}
      (hInit : s.findInitializing? key = some { runtimeId := runtimeId, key := key })
      (hNoTopic : s.findTopic? key = none)
      (hNoToken : ∀ topic ∈ s.byKey, topic.token ≠ token)
      (hRuntime : Runtime.Step s.runtime
        (.insertPendingReuse runtimeId slot generation) runtime')
      (hPending : runtime'.findInitializer? runtimeId =
        some { id := runtimeId, stage := .pending token })
      (hRoot : Runtime.TokenLive runtime'.registry token) :
      Step s (.publishVisibleReuse key runtimeId slot generation)
        { s with
            runtime := runtime'
            byKey := s.byKey ++ [{ key := key, token := token, stage := .provisional }] }

  | commitPublication
      {s : State} {runtime' : Runtime.State} {topic : Topic}
      {key : TopicKey} {runtimeId : Runtime.InitializerId}
      (hInit : s.findInitializing? key = some { runtimeId := runtimeId, key := key })
      (hTopic : s.findTopic? key = some { topic with stage := .provisional })
      (hTopicKey : topic.key = key)
      (hPending : s.runtime.findInitializer? runtimeId =
        some { id := runtimeId, stage := .pending topic.token })
      (hRuntime : Runtime.Step s.runtime (.publishTopic runtimeId) runtime') :
      Step s (.commitPublication key runtimeId)
        { s with
            runtime := runtime'
            byKey := s.updateTopicStage key .committed }

  | rollbackVisibleReuse
      {s : State} {runtime' : Runtime.State} {topic : Topic}
      {key : TopicKey} {runtimeId : Runtime.InitializerId}
      {nextGeneration : Registry.Generation}
      (hInit : s.findInitializing? key = some { runtimeId := runtimeId, key := key })
      (hTopic : s.findTopic? key = some { topic with stage := .provisional })
      (hTopicKey : topic.key = key)
      (hPending : s.runtime.findInitializer? runtimeId =
        some { id := runtimeId, stage := .pending topic.token })
      (hRuntime : Runtime.Step s.runtime
        (.rollbackPendingReuse runtimeId nextGeneration) runtime') :
      Step s (.rollbackVisibleReuse key runtimeId nextGeneration)
        { s with
            runtime := runtime'
            byKey := s.removeTopic key }

  | rollbackVisibleRetire
      {s : State} {runtime' : Runtime.State} {topic : Topic}
      {key : TopicKey} {runtimeId : Runtime.InitializerId}
      (hInit : s.findInitializing? key = some { runtimeId := runtimeId, key := key })
      (hTopic : s.findTopic? key = some { topic with stage := .provisional })
      (hTopicKey : topic.key = key)
      (hPending : s.runtime.findInitializer? runtimeId =
        some { id := runtimeId, stage := .pending topic.token })
      (hRuntime : Runtime.Step s.runtime
        (.rollbackPendingRetire runtimeId) runtime') :
      Step s (.rollbackVisibleRetire key runtimeId)
        { s with
            runtime := runtime'
            byKey := s.removeTopic key }

  | finishInitializer
      {s : State} {runtime' : Runtime.State}
      {key : TopicKey} {runtimeId : Runtime.InitializerId}
      (hInit : s.findInitializing? key = some { runtimeId := runtimeId, key := key })
      (hReady : ∀ topic ∈ s.byKey, topic.key = key → topic.stage = .committed)
      (hRuntime : Runtime.Step s.runtime (.finishInitialize runtimeId) runtime') :
      Step s (.finishInitializer key runtimeId)
        { s with
            runtime := runtime'
            initializing := s.removeInitializing runtimeId }

inductive Reachable : State → State → Prop where
  | refl (s : State) : Reachable s s
  | tail {s t u : State} {e : Event} : Reachable s t → Step t e u → Reachable s u

end XlFnFormal.Handle.Topics
