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
  | beginFastLookup (readerId : Nat) (token : Token)
  | completeFastLookup (readerId : Nat)
  | fallbackFastLookup (readerId : Nat)
  | beginSlowLookup (token : Token)
  | endSlowLookup
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
            publications := s.publications ++ [{ slot := s.registry.slots.length, generation := 1, state := .live }]
            snapshot := s.snapshot ++ [{ slot := s.registry.slots.length, generation := 1 }] }

  | insertReuse
      {s : State} {reg' : Registry.State}
      {slot : SlotId} {generation : Generation}
      (hReg : Registry.Step s.registry (.insertReuse slot generation) reg')
      (hNoSnap : s.findSnapshot? slot = none)
      (hNoPub : s.findPublication? slot generation = none) :
      Step s (.insertReuse slot generation)
        { s with
            registry := reg'
            publications := s.publications ++ [{ slot := slot, generation := generation, state := .live }]
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

  | beginFastLookup
      {s : State} {reg' : Registry.State}
      {readerId : Nat} {token : Token} {pub : Publication} {binding : SnapshotBinding}
      (hNoReader : s.findFastLookup? readerId = none)
      (hSnap : s.findSnapshot? token.slot = some binding)
      (hSnapGen : binding.generation = token.generation)
      (hPub : s.findPublication? token.slot token.generation = some pub)
      (hLive : pub.state = .live)
      (hReg : Registry.Step s.registry (.beginLookup token) reg') :
      Step s (.beginFastLookup readerId token)
        { s with
            registry := reg'
            fastLookups := s.fastLookups ++ [{ id := readerId, token := token }] }

  | completeFastLookup
      {s : State} {reg' : Registry.State}
      {readerId : Nat} {lookup : FastLookup}
      (hLookup : s.findFastLookup? readerId = some lookup)
      (hReg : Registry.Step s.registry .endLookup reg') :
      Step s (.completeFastLookup readerId)
        { s with
            registry := reg'
            fastLookups := s.removeFastLookup readerId }

  | fallbackFastLookup
      {s : State} {reg' : Registry.State}
      {readerId : Nat} {lookup : FastLookup}
      (hLookup : s.findFastLookup? readerId = some lookup)
      (hReg : Registry.Step s.registry .endLookup reg') :
      Step s (.fallbackFastLookup readerId)
        { s with
            registry := reg'
            fastLookups := s.removeFastLookup readerId }

  | beginSlowLookup
      {s : State} {reg' : Registry.State}
      {token : Token}
      (hReg : Registry.Step s.registry (.beginLookup token) reg') :
      Step s (.beginSlowLookup token)
        { s with registry := reg' }

  | endSlowLookup
      {s : State} {reg' : Registry.State}
      (hSlowLease : s.fastLookups.length < s.registry.activeLeases)
      (hReg : Registry.Step s.registry .endLookup reg') :
      Step s .endSlowLookup
        { s with registry := reg' }

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
      (hReg : Registry.Step s.registry .finishClose reg') :
      Step s .finishClose s

inductive Reachable : State → State → Prop where
  | refl (s : State) : Reachable s s
  | tail {s t u : State} {e : Event} : Reachable s t → Step t e u → Reachable s u

end XlFnFormal.Handle.Registry.Snapshot
