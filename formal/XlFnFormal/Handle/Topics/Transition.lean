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
  | insertPendingFresh (key : TopicKey) (runtimeId : Runtime.InitializerId)
  | insertPendingReuse (key : TopicKey) (runtimeId : Runtime.InitializerId)
      (slot : Registry.SlotId) (generation : Registry.Generation)
  | publishVisible (key : TopicKey) (runtimeId : Runtime.InitializerId) (rtdKey : RtdKey)
  | commitPublication (key : TopicKey) (runtimeId : Runtime.InitializerId)
  | withdrawVisible (key : TopicKey) (runtimeId : Runtime.InitializerId)
  | rollbackPendingReuse (key : TopicKey) (runtimeId : Runtime.InitializerId)
      (nextGeneration : Registry.Generation)
  | rollbackPendingRetire (key : TopicKey) (runtimeId : Runtime.InitializerId)
  | finishInitializer (key : TopicKey) (runtimeId : Runtime.InitializerId)
  | closeRegistry
  | finishClose
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
      Step s .sealTopics { s with runtime := runtime', byKey := [], byRtdKey := [] }

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

  | insertPendingFresh
      {s : State} {runtime' : Runtime.State}
      {key : TopicKey} {runtimeId : Runtime.InitializerId}
      (hInit : s.findInitializing? key = some { runtimeId := runtimeId, key := key })
      (hNoTopic : s.findTopic? key = none)
      (hRuntime : Runtime.Step s.runtime (.insertPendingFresh runtimeId) runtime') :
      Step s (.insertPendingFresh key runtimeId) { s with runtime := runtime' }

  | insertPendingReuse
      {s : State} {runtime' : Runtime.State}
      {key : TopicKey} {runtimeId : Runtime.InitializerId}
      {slot : Registry.SlotId} {generation : Registry.Generation}
      (hInit : s.findInitializing? key = some { runtimeId := runtimeId, key := key })
      (hNoTopic : s.findTopic? key = none)
      (hRuntime : Runtime.Step s.runtime
        (.insertPendingReuse runtimeId slot generation) runtime') :
      Step s (.insertPendingReuse key runtimeId slot generation)
        { s with runtime := runtime' }

  | publishVisible
      {s : State} {token : Registry.Token}
      {key : TopicKey} {runtimeId : Runtime.InitializerId} {rtdKey : RtdKey}
      (hPhase : s.runtime.phase = .open)
      (hInit : s.findInitializing? key = some { runtimeId := runtimeId, key := key })
      (hNoTopic : s.findTopic? key = none)
      (hNoRtdKey : s.findReverse? rtdKey = none)
      (hNoToken : ∀ topic ∈ s.byKey, topic.token ≠ token)
      (hPending : s.runtime.findInitializer? runtimeId =
        some { id := runtimeId, stage := .pending token })
      (hRoot : Runtime.TokenLive s.runtime.registry token) :
      Step s (.publishVisible key runtimeId rtdKey)
        { s with
            byKey := s.byKey ++ [{ key := key, rtdKey := rtdKey, token := token, stage := .provisional }]
            byRtdKey := s.byRtdKey ++ [{ rtdKey := rtdKey, key := key }] }

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

  | withdrawVisible
      {s : State} {topic : Topic}
      {key : TopicKey} {runtimeId : Runtime.InitializerId}
      (hInit : s.findInitializing? key = some { runtimeId := runtimeId, key := key })
      (hTopic : s.findTopic? key = some { topic with stage := .provisional })
      (hTopicKey : topic.key = key)
      (hPending : s.runtime.findInitializer? runtimeId =
        some { id := runtimeId, stage := .pending topic.token }) :
      Step s (.withdrawVisible key runtimeId)
        { s with
            byKey := s.removeTopic key
            byRtdKey := s.removeReverse topic.rtdKey }

  | rollbackPendingReuse
      {s : State} {runtime' : Runtime.State}
      {token : Registry.Token}
      {key : TopicKey} {runtimeId : Runtime.InitializerId}
      {nextGeneration : Registry.Generation}
      (hInit : s.findInitializing? key = some { runtimeId := runtimeId, key := key })
      (hNoTopic : s.findTopic? key = none)
      (hNoToken : ∀ topic ∈ s.byKey, topic.token ≠ token)
      (hPending : s.runtime.findInitializer? runtimeId =
        some { id := runtimeId, stage := .pending token })
      (hRuntime : Runtime.Step s.runtime
        (.rollbackPendingReuse runtimeId nextGeneration) runtime') :
      Step s (.rollbackPendingReuse key runtimeId nextGeneration)
        { s with runtime := runtime' }

  | rollbackPendingRetire
      {s : State} {runtime' : Runtime.State}
      {token : Registry.Token}
      {key : TopicKey} {runtimeId : Runtime.InitializerId}
      (hInit : s.findInitializing? key = some { runtimeId := runtimeId, key := key })
      (hNoTopic : s.findTopic? key = none)
      (hNoToken : ∀ topic ∈ s.byKey, topic.token ≠ token)
      (hPending : s.runtime.findInitializer? runtimeId =
        some { id := runtimeId, stage := .pending token })
      (hRuntime : Runtime.Step s.runtime (.rollbackPendingRetire runtimeId) runtime') :
      Step s (.rollbackPendingRetire key runtimeId)
        { s with runtime := runtime' }

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

  | closeRegistry
      {s : State} {runtime' : Runtime.State}
      (hNoVisible : s.byKey = [])
      (hNoReverse : s.byRtdKey = [])
      (hNoInitializers : s.initializing = [])
      (hRuntime : Runtime.Step s.runtime .closeRegistry runtime') :
      Step s .closeRegistry { s with runtime := runtime' }

  | finishClose
      {s : State} {runtime' : Runtime.State}
      (hRuntime : Runtime.Step s.runtime .finishClose runtime') :
      Step s .finishClose { s with runtime := runtime' }

inductive Reachable : State → State → Prop where
  | refl (s : State) : Reachable s s
  | tail {s t u : State} {e : Event} : Reachable s t → Step t e u → Reachable s u

end XlFnFormal.Handle.Topics
