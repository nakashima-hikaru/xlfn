import XlFnFormal.Handle.Model

set_option autoImplicit false

namespace XlFnFormal.Handle

inductive Event where
  | beginPrepare
  | endPrepare
  | beginInitialize
  | finishInitialize
  | publishTopic
  | rollbackPending
  | insertFresh
  | insertReuse (slot : SlotId) (generation : Generation)
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
      (hPrep : s.activePrepares > 0) :
      Step s .endPrepare { s with activePrepares := s.activePrepares - 1 }

  | beginInitialize
      {s : State}
      (hPhase : s.phase = .«open» ∨ s.phase = .drainingPrepares)
      (hPrep : s.activePrepares > 0) :
      Step s .beginInitialize { s with activeInitializers := s.activeInitializers + 1 }

  | finishInitialize
      {s : State}
      (hInit : s.activeInitializers > 0) :
      Step s .finishInitialize { s with activeInitializers := s.activeInitializers - 1 }

  | publishTopic
      {s : State}
      (hPhase : s.phase = .«open»)
      (hInit : s.activeInitializers > 0) :
      Step s .publishTopic s

  | rollbackPending
      {s : State}
      (hInit : s.activeInitializers > 0) :
      Step s .rollbackPending s

  | insertFresh
      {s : State}
      (hMay : s.MayInsert) :
      Step s .insertFresh
        { s with slots := s.slots ++ [.live 1] }

  | insertReuse
      {s : State}
      {slotId : SlotId}
      {gen : Generation}
      (hMay : s.MayInsert)
      (hInBounds : slotId < s.slots.length)
      (hVacant : s.slots.get ⟨slotId, hInBounds⟩ = .vacant gen) :
      Step s (.insertReuse slotId gen)
        { s with slots := s.slots.set slotId (.live gen) }

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
      (hNoInits : s.activeInitializers = 0)
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
