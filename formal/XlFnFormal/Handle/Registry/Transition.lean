import XlFnFormal.Handle.Registry.Model

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Registry

inductive Event where
  | insertFresh
  | insertReuse (slot : SlotId) (generation : Generation)
  | removeReuse (token : Token) (nextGeneration : Generation)
  | removeRetire (token : Token)
  | beginLookup (token : Token)
  | endLookup
  | closeRegistry
  | finishClose
deriving DecidableEq, Repr

inductive Step : State → Event → State → Prop where
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
      (hNotClosed : s.closed = false)
      (hAuth : s.AuthenticatedFor token)
      (hInBounds : token.slot < s.slots.length)
      (hLive : s.slots.get ⟨token.slot, hInBounds⟩ = .live token.generation) :
      Step s (.beginLookup token)
        { s with activeBorrows := s.activeBorrows + 1 }

  | endLookup
      {s : State}
      (hBorrows : s.activeBorrows > 0) :
      Step s .endLookup
        { s with activeBorrows := s.activeBorrows - 1 }

  | closeRegistry
      {s : State}
      (hNotClosed : s.closed = false) :
      Step s .closeRegistry
        { s with
            closed := true
            slots := s.slots.map closeSlot }

  | finishClose
      {s : State}
      (hClosed : s.closed)
      (hNoBorrows : s.activeBorrows = 0) :
      Step s .finishClose s

inductive Reachable : State → State → Prop where
  | refl (s : State) : Reachable s s
  | tail {s t u : State} {e : Event} : Reachable s t → Step t e u → Reachable s u

end XlFnFormal.Handle.Registry
