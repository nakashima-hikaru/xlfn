import XlFnFormal.Handle.Registry.Snapshot.Model

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Registry.Snapshot

open XlFnFormal.Handle.Registry

inductive Event where
  | insertFresh
  | insertReuse (slot : SlotId) (generation : Generation)
  | removeReuse (token : Token) (nextGeneration : Generation)
  | removeRetire (token : Token)
  | observeBorrow (readerId : Nat) (token : Token)
  | releaseBorrow (readerId : Nat)
  | retirePublication (slot : SlotId) (generation : Generation)
  | closeRegistry
  | finishClose
deriving DecidableEq, Repr

inductive Step : State → Event → State → Prop where
  | insertFresh
      {s : State} {reg' : Registry.State}
      (hReg : Registry.Step s.registry .insertFresh reg')
      (hNoSnap : s.findSnapshot? s.registry.slots.length = none)
      (hNoPub : s.findPublication? s.registry.slots.length 1 = none) :
      Step s .insertFresh
        { s with
            registry := reg'
            publications := s.publications ++
              [{ slot := s.registry.slots.length, generation := 1, state := .live }]
            snapshot := s.snapshot ++
              [{ slot := s.registry.slots.length, generation := 1 }] }

  | insertReuse
      {s : State} {reg' : Registry.State}
      {slot : SlotId} {generation : Generation}
      (hReg : Registry.Step s.registry (.insertReuse slot generation) reg')
      (hNoSnap : s.findSnapshot? slot = none)
      (hNoPub : s.findPublication? slot generation = none) :
      Step s (.insertReuse slot generation)
        { s with
            registry := reg'
            publications := s.publications ++
              [{ slot := slot, generation := generation, state := .live }]
            snapshot := s.snapshot ++ [{ slot := slot, generation := generation }] }

  | removeReuse
      {s : State} {reg' : Registry.State}
      {token : Token} {nextGen : Generation} {pub : Publication}
      (hReg : Registry.Step s.registry (.removeReuse token nextGen) reg')
      (hPub : s.findPublication? token.slot token.generation = some pub)
      (hLive : pub.state = .live) :
      Step s (.removeReuse token nextGen)
        { s with
            registry := reg'
            publications := s.updatePublicationState token.slot token.generation .stale
            snapshot := s.removeSnapshot token.slot }

  | removeRetire
      {s : State} {reg' : Registry.State}
      {token : Token} {pub : Publication}
      (hReg : Registry.Step s.registry (.removeRetire token) reg')
      (hPub : s.findPublication? token.slot token.generation = some pub)
      (hLive : pub.state = .live) :
      Step s (.removeRetire token)
        { s with
            registry := reg'
            publications := s.updatePublicationState token.slot token.generation .stale
            snapshot := s.removeSnapshot token.slot }

  | observeBorrow
      {s : State} {reg' : Registry.State}
      {readerId : Nat} {token : Token} {binding : SnapshotBinding} {pub : Publication}
      (hNoReader : s.findBorrow? readerId = none)
      (hSnap : s.findSnapshot? token.slot = some binding)
      (hSnapGen : binding.generation = token.generation)
      (hPub : s.findPublication? token.slot token.generation = some pub)
      (hAuth : token.session = s.registry.session)
      (hLive : pub.state = .live)
      (hReg : Registry.Step s.registry (.beginLookup token) reg') :
      Step s (.observeBorrow readerId token)
        { s with
            registry := reg'
            borrows := s.borrows ++ [{ id := readerId, token := token }] }

  | releaseBorrow
      {s : State} {reg' : Registry.State} {readerId : Nat} {borrow : Borrow}
      (hBorrow : s.findBorrow? readerId = some borrow)
      (hReg : Registry.Step s.registry .endLookup reg') :
      Step s (.releaseBorrow readerId)
        { s with
            registry := reg'
            borrows := s.borrows.filter (fun b => b.id != readerId) }

  | retirePublication
      {s : State} {slot : SlotId} {generation : Generation} {pub : Publication}
      (hPub : s.findPublication? slot generation = some pub)
      (hNotLive : pub.state ≠ .live)
      (hNoSnapshot : s.findSnapshot? slot = none)
      (hNoBorrow : s.findBorrowFor? slot generation = none) :
      Step s (.retirePublication slot generation)
        { s with publications := s.removePublication slot generation }

  | closeRegistry
      {s : State} {reg' : Registry.State}
      (hReg : Registry.Step s.registry .closeRegistry reg') :
      Step s .closeRegistry
        { s with
            registry := reg'
            publications := s.updateClosingPublications
            snapshot := [] }

  | finishClose
      {s : State} {reg' : Registry.State}
      (hNoBorrows : s.borrows = [])
      (hNoPublications : s.publications = [])
      (hNoSnapshot : s.snapshot = [])
      (hReg : Registry.Step s.registry .finishClose reg') :
      Step s .finishClose s

inductive Reachable : State → State → Prop where
  | refl (s : State) : Reachable s s
  | tail {s t u : State} {e : Event} :
      Reachable s t → Step t e u → Reachable s u

end XlFnFormal.Handle.Registry.Snapshot
