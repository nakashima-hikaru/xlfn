import XlFnFormal.Handle.Publication.Model

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Publication

open XlFnFormal.Handle.Registry

inductive Event where
  | insertFresh
  | insertReuse (slot : SlotId) (generation : Generation)
  | enterReader (slot : SlotId) (generation : Generation)
  | releaseReader (slot : SlotId) (generation : Generation)
  | acquirePin (objectId : Nat)
  | releasePin (objectId : Nat)
  | beginRetire (slot : SlotId) (generation : Generation)
  | retireCapability (slot : SlotId) (generation : Generation)
  | reclaimObject (objectId : Nat)
  | removeReuse (token : Token) (nextGeneration : Generation)
  | removeRetire (token : Token)
  | closeRegistry
  | finishClose
deriving DecidableEq, Repr

inductive Step : State → Event → State → Prop where
  | insertFresh
      {s : State} {reg' : Registry.State}
      (hReg : Registry.Step s.registry .insertFresh reg')
      (hNoPub : s.findPublication? s.registry.slots.length 1 = none) :
      Step s .insertFresh
        { s with
            registry := reg'
            publications := s.publications ++
              [{ slot := s.registry.slots.length, generation := 1, state := .live,
                 gate := .open, admitted := 0, published := true, owned := true,
                 objectId := s.nextObjectId }]
            objects := s.objects ++
              [{ id := s.nextObjectId, present := true, bindings := 1, pins := 0, retired := false }]
            nextObjectId := s.nextObjectId + 1 }

  | insertReuse
      {s : State} {reg' : Registry.State}
      {slot : SlotId} {generation : Generation}
      (hReg : Registry.Step s.registry (.insertReuse slot generation) reg')
      (hNoPub : s.findPublication? slot generation = none) :
      Step s (.insertReuse slot generation)
        { s with
            registry := reg'
            publications := s.publications ++
              [{ slot := slot, generation := generation, state := .live,
                 gate := .open, admitted := 0, published := true, owned := true,
                 objectId := s.nextObjectId }]
            objects := s.objects ++
              [{ id := s.nextObjectId, present := true, bindings := 1, pins := 0, retired := false }]
            nextObjectId := s.nextObjectId + 1 }

  | enterReader
      {s : State} {slot : SlotId} {generation : Generation} {pub : Publication}
      (hPub : s.findPublication? slot generation = some pub)
      (hGate : pub.gate = .open)
      (hPubVis : pub.published = true)
      (hOwned : pub.owned = true) :
      Step s (.enterReader slot generation)
        { s with
            publications := s.updatePublication slot generation
              (fun p => { p with admitted := p.admitted + 1 }) }

  | releaseReader
      {s : State} {slot : SlotId} {generation : Generation} {pub : Publication}
      (hPub : s.findPublication? slot generation = some pub)
      (hAdm : pub.admitted > 0) :
      Step s (.releaseReader slot generation)
        { s with
            publications := s.updatePublication slot generation
              (fun p => { p with admitted := p.admitted - 1 }) }

  | acquirePin
      {s : State} {objectId : Nat} {obj : ObjectState}
      (hObj : s.findObject? objectId = some obj)
      (hPresent : obj.present = true)
      (hNotRet : obj.retired = false) :
      Step s (.acquirePin objectId)
        { s with
            objects := s.updateObject objectId
              (fun o => { o with pins := o.pins + 1 }) }

  | releasePin
      {s : State} {objectId : Nat} {obj : ObjectState}
      (hObj : s.findObject? objectId = some obj)
      (hPins : obj.pins > 0) :
      Step s (.releasePin objectId)
        { s with
            objects := s.updateObject objectId
              (fun o => { o with pins := o.pins - 1 }) }

  | beginRetire
      {s : State} {slot : SlotId} {generation : Generation} {pub : Publication}
      (hPub : s.findPublication? slot generation = some pub)
      (hPubVis : pub.published = true) :
      Step s (.beginRetire slot generation)
        { s with
            publications := s.updatePublication slot generation
              (fun p => { p with published := false, gate := .sealed, state := .stale }) }

  | retireCapability
      {s : State} {slot : SlotId} {generation : Generation} {pub : Publication}
      (hPub : s.findPublication? slot generation = some pub)
      (hNotPub : pub.published = false)
      (hSealed : pub.gate = .sealed)
      (hDrained : pub.admitted = 0)
      (hOwned : pub.owned = true) :
      Step s (.retireCapability slot generation)
        { s with
            publications := s.updatePublication slot generation
              (fun p => { p with owned := false, state := .retired })
            objects := s.updateObject pub.objectId
              (fun o => { o with bindings := o.bindings - 1, retired := true }) }

  | reclaimObject
      {s : State} {objectId : Nat} {obj : ObjectState}
      (hObj : s.findObject? objectId = some obj)
      (hCan : objectCanReclaim obj)
      (hPresent : obj.present = true) :
      Step s (.reclaimObject objectId)
        { s with
            objects := s.updateObject objectId
              (fun o => { o with present := false }) }

  | removeReuse
      {s : State} {reg' : Registry.State}
      {token : Token} {nextGen : Generation} {pub : Publication}
      (hReg : Registry.Step s.registry (.removeReuse token nextGen) reg')
      (hPub : s.findPublication? token.slot token.generation = some pub) :
      Step s (.removeReuse token nextGen)
        { s with registry := reg' }

  | removeRetire
      {s : State} {reg' : Registry.State}
      {token : Token} {pub : Publication}
      (hReg : Registry.Step s.registry (.removeRetire token) reg')
      (hPub : s.findPublication? token.slot token.generation = some pub) :
      Step s (.removeRetire token)
        { s with registry := reg' }

  | closeRegistry
      {s : State} {reg' : Registry.State}
      (hReg : Registry.Step s.registry .closeRegistry reg') :
      Step s .closeRegistry
        { s with
            registry := reg'
            publications := s.publications.map (fun p =>
              if p.state = .live then
                { p with state := .closing, gate := .sealed, published := false }
              else p) }

  | finishClose
      {s : State} {reg' : Registry.State}
      (hDrained : ∀ p ∈ s.publications, p.admitted = 0 ∧ p.owned = false)
      (hReclaimed : ∀ o ∈ s.objects, o.present = false)
      (hReg : Registry.Step s.registry .finishClose reg') :
      Step s .finishClose s

inductive Reachable : State → State → Prop where
  | refl (s : State) : Reachable s s
  | tail {s t u : State} {e : Event} :
      Reachable s t → Step t e u → Reachable s u

end XlFnFormal.Handle.Publication
