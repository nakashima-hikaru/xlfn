import XlFnFormal.Handle.Model

set_option autoImplicit false

namespace XlFnFormal.Handle

inductive Event where
  | beginPrepare
  | endPrepare
  | insert (slot : SlotId) (generation : Generation)
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
      (hPhase : s.phase = .«open») :
      Step s .beginPrepare { s with activePrepares := s.activePrepares + 1 }

  | endPrepare
      {s : State}
      (hPrep : s.activePrepares > 0) :
      Step s .endPrepare { s with activePrepares := s.activePrepares - 1 }

  | insert
      {s : State}
      {slotId : SlotId}
      {gen : Generation}
      (hPhase : s.phase = .«open» ∨ s.phase = .drainingPrepares)
      (hInBounds : slotId < s.slots.length)
      (hVacant : s.slots.get ⟨slotId, hInBounds⟩ = .vacant gen) :
      Step s (.insert slotId gen)
        { s with slots := s.slots.set slotId (.live gen) }

  | removeReuse
      {s : State}
      {token : Token}
      {nextGen : Generation}
      (hAuth : s.AuthenticatedFor token)
      (hInBounds : token.slot < s.slots.length)
      (hLive : s.slots.get ⟨token.slot, hInBounds⟩ = .live token.generation)
      (hNextGen : nextGen = token.generation + 1) :
      Step s (.removeReuse token nextGen)
        { s with slots := s.slots.set token.slot (.vacant nextGen) }

  | removeRetire
      {s : State}
      {token : Token}
      (hAuth : s.AuthenticatedFor token)
      (hInBounds : token.slot < s.slots.length)
      (hLive : s.slots.get ⟨token.slot, hInBounds⟩ = .live token.generation) :
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
      (hNoPrepares : s.activePrepares = 0) :
      Step s .closeRegistry
        { s with
            phase := .registryClosed
            slots := s.slots.map (fun slot =>
              match slot with
              | .live g => SlotState.vacant (g + 1)
              | other => other) }

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
