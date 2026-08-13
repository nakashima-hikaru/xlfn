import XlFnFormal.Handle.Runtime.Model
import XlFnFormal.Handle.Registry.Transition

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Runtime

open Registry (SessionId SlotId Generation Token SlotState closeSlot maxGeneration nextGeneration?)

inductive Event where
  | beginPrepare
  | endPrepare
  | beginInitialize (id : InitializerId)
  | finishInitialize (id : InitializerId)
  | insertPendingFresh (id : InitializerId)
  | insertPendingReuse (id : InitializerId) (slot : SlotId) (generation : Generation)
  | publishTopic (id : InitializerId)
  | rollbackPendingReuse (id : InitializerId) (nextGeneration : Generation)
  | rollbackPendingRetire (id : InitializerId)
  | beginLookup (token : Token)
  | endLookup
  | sealTopics
  | closeRegistry
  | finishClose
deriving DecidableEq, Repr

inductive Step : State → Event → State → Prop where
  | beginPrepare
      {s : State}
      (hPhase : s.phase = .«open» ∨ s.phase = .drainingPrepares) :
      Step s .beginPrepare { s with activePrepares := s.activePrepares + 1 }

  | endPrepare
      {s : State}
      (hPrep : s.activePrepares > s.initializers.length) :
      Step s .endPrepare { s with activePrepares := s.activePrepares - 1 }

  | beginInitialize
      {s : State}
      {id : InitializerId}
      (hPhase : s.phase = .«open»)
      (hPrep : s.activePrepares > s.initializers.length)
      (hFresh : s.findInitializer? id = none) :
      Step s (.beginInitialize id)
        { s with initializers := s.initializers ++ [{ id := id, stage := .beforeInsert }] }

  | finishInitialize
      {s : State}
      {id : InitializerId}
      {init : Initializer}
      (hFind : s.findInitializer? id = some init)
      (hStage : init.stage = .beforeInsert ∨ init.stage = .resolved) :
      Step s (.finishInitialize id)
        { s with initializers := s.removeInitializer id }

  | insertPendingFresh
      {s : State}
      {id : InitializerId}
      {reg' : Registry.State}
      (hPhase : s.phase = .«open»)
      (hFind : s.findInitializer? id = some { id := id, stage := .beforeInsert })
      (hRegStep : Registry.Step s.registry .insertFresh reg') :
      Step s (.insertPendingFresh id)
        { s with
            registry := reg'
            initializers := s.updateInitializer id (.pending { session := s.registry.session, slot := s.registry.slots.length, generation := 1 }) }

  | insertPendingReuse
      {s : State}
      {id : InitializerId}
      {slotId : SlotId}
      {gen : Generation}
      {reg' : Registry.State}
      (hPhase : s.phase = .«open»)
      (hFind : s.findInitializer? id = some { id := id, stage := .beforeInsert })
      (hRegStep : Registry.Step s.registry (.insertReuse slotId gen) reg') :
      Step s (.insertPendingReuse id slotId gen)
        { s with
            registry := reg'
            initializers := s.updateInitializer id (.pending { session := s.registry.session, slot := slotId, generation := gen }) }

  | publishTopic
      {s : State}
      {id : InitializerId}
      {token : Token}
      (hPhase : s.phase = .«open»)
      (hFind : s.findInitializer? id = some { id := id, stage := .pending token }) :
      Step s (.publishTopic id)
        { s with initializers := s.updateInitializer id .resolved }

  | rollbackPendingReuse
      {s : State}
      {id : InitializerId}
      {token : Token}
      {nextGen : Generation}
      {reg' : Registry.State}
      (hFind : s.findInitializer? id = some { id := id, stage := .pending token })
      (hRegStep : Registry.Step s.registry (.removeReuse token nextGen) reg') :
      Step s (.rollbackPendingReuse id nextGen)
        { s with
            registry := reg'
            initializers := s.updateInitializer id .resolved }

  | rollbackPendingRetire
      {s : State}
      {id : InitializerId}
      {token : Token}
      {reg' : Registry.State}
      (hFind : s.findInitializer? id = some { id := id, stage := .pending token })
      (hRegStep : Registry.Step s.registry (.removeRetire token) reg') :
      Step s (.rollbackPendingRetire id)
        { s with
            registry := reg'
            initializers := s.updateInitializer id .resolved }

  | beginLookup
      {s : State}
      {token : Token}
      {reg' : Registry.State}
      (hRegStep : Registry.Step s.registry (.beginLookup token) reg') :
      Step s (.beginLookup token)
        { s with registry := reg' }

  | endLookup
      {s : State}
      {reg' : Registry.State}
      (hRegStep : Registry.Step s.registry .endLookup reg') :
      Step s .endLookup
        { s with registry := reg' }

  | sealTopics
      {s : State}
      (hPhase : s.phase = .«open») :
      Step s .sealTopics { s with phase := .drainingPrepares }

  | closeRegistry
      {s : State}
      {reg' : Registry.State}
      (hPhase : s.phase = .drainingPrepares)
      (hNoInits : s.initializers = [])
      (hNoPrepares : s.activePrepares = 0)
      (hRegStep : Registry.Step s.registry .closeRegistry reg') :
      Step s .closeRegistry
        { s with
            phase := .registryClosed
            registry := reg' }

  | finishClose
      {s : State}
      {reg' : Registry.State}
      (hPhase : s.phase = .registryClosed)
      (hRegStep : Registry.Step s.registry .finishClose reg') :
      Step s .finishClose
        { s with phase := .closed }

inductive Reachable : State → State → Prop where
  | refl (s : State) : Reachable s s
  | tail {s t u : State} {e : Event} : Reachable s t → Step t e u → Reachable s u

end XlFnFormal.Handle.Runtime
