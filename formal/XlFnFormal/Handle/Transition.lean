import XlFnFormal.Handle.Model

set_option autoImplicit false

namespace XlFnFormal.Handle

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
  | removeReuse (token : Token) (nextGeneration : Generation)
  | removeRetire (token : Token)
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
      (hFind : s.findInitializer? id = some { id := id, stage := .beforeInsert }) :
      Step s (.insertPendingFresh id)
        { s with
            slots := s.slots ++ [.live 1]
            initializers := s.updateInitializer id (.pending { session := s.session, slot := s.slots.length, generation := 1 }) }

  | insertPendingReuse
      {s : State}
      {id : InitializerId}
      {slotId : SlotId}
      {gen : Generation}
      (hFind : s.findInitializer? id = some { id := id, stage := .beforeInsert })
      (hInBounds : slotId < s.slots.length)
      (hVacant : s.slots.get ⟨slotId, hInBounds⟩ = .vacant gen) :
      Step s (.insertPendingReuse id slotId gen)
        { s with
            slots := s.slots.set slotId (.live gen)
            initializers := s.updateInitializer id (.pending { session := s.session, slot := slotId, generation := gen }) }

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
      (hFind : s.findInitializer? id = some { id := id, stage := .pending token })
      (hInBounds : token.slot < s.slots.length)
      (hLive : s.slots.get ⟨token.slot, hInBounds⟩ = .live token.generation)
      (hNextGen : nextGeneration? token.generation = some nextGen) :
      Step s (.rollbackPendingReuse id nextGen)
        { s with
            slots := s.slots.set token.slot (.vacant nextGen)
            initializers := s.updateInitializer id .resolved }

  | rollbackPendingRetire
      {s : State}
      {id : InitializerId}
      {token : Token}
      (hFind : s.findInitializer? id = some { id := id, stage := .pending token })
      (hInBounds : token.slot < s.slots.length)
      (hLive : s.slots.get ⟨token.slot, hInBounds⟩ = .live token.generation)
      (hExhausted : nextGeneration? token.generation = none) :
      Step s (.rollbackPendingRetire id)
        { s with
            slots := s.slots.set token.slot .retired
            initializers := s.updateInitializer id .resolved }

  | removeReuse
      {s : State}
      {token : Token}
      {nextGen : Generation}
      (hAuth : s.AuthenticatedFor token)
      (hInBounds : token.slot < s.slots.length)
      (hLive : s.slots.get ⟨token.slot, hInBounds⟩ = .live token.generation)
      (hNextGen : nextGeneration? token.generation = some nextGen) :
      Step s (.removeReuse token nextGen)
        { s with slots := s.slots.set token.slot (.vacant nextGen) }

  | removeRetire
      {s : State}
      {token : Token}
      (hAuth : s.AuthenticatedFor token)
      (hInBounds : token.slot < s.slots.length)
      (hLive : s.slots.get ⟨token.slot, hInBounds⟩ = .live token.generation)
      (hExhausted : nextGeneration? token.generation = none) :
      Step s (.removeRetire token)
        { s with slots := s.slots.set token.slot .retired }

  | beginLookup
      {s : State}
      {token : Token}
      (hPhase : s.phase ≠ .closed)
      (hAuth : s.AuthenticatedFor token)
      (hInBounds : token.slot < s.slots.length)
      (hLive : s.slots.get ⟨token.slot, hInBounds⟩ = .live token.generation) :
      Step s (.beginLookup token)
        { s with activeLeases := s.activeLeases + 1 }

  | endLookup
      {s : State}
      (hLease : s.activeLeases > 0) :
      Step s .endLookup
        { s with activeLeases := s.activeLeases - 1 }

  | sealTopics
      {s : State}
      (hPhase : s.phase = .«open») :
      Step s .sealTopics
        { s with phase := .drainingPrepares }

  | closeRegistry
      {s : State}
      (hPhase : s.phase = .«open» ∨ s.phase = .drainingPrepares)
      (hNoInits : s.initializers = [])
      (hNoPrepares : s.activePrepares = 0) :
      Step s .closeRegistry
        { s with
            phase := .registryClosed
            slots := s.slots.map closeSlot }

  | finishClose
      {s : State}
      (hPhase : s.phase = .registryClosed)
      (hNoLeases : s.activeLeases = 0) :
      Step s .finishClose
        { s with phase := .closed }

inductive Reachable (init : State) : State → Prop where
  | init : Reachable init init
  | step {s s' : State} {e : Event} (hReach : Reachable init s) (hStep : Step s e s') : Reachable init s'

end XlFnFormal.Handle
