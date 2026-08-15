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
  | beginFastObservation (readerId : Nat) (token : Token)
  | acquireTentativeLease (readerId : Nat)
  | abandonObservation (readerId : Nat)
  | validateFastLookup (readerId : Nat)
  | rejectTentativeFastLookup (readerId : Nat)
  | completeFastLookup (readerId : Nat)
  | fallbackFastLookup (readerId : Nat)
  | beginSlowLookup (token : Token)
  | endSlowLookup
  | beginSealLeaseAdmission
  | finishSealLeaseAdmission
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

  | beginFastObservation
      {s : State}
      {readerId : Nat} {token : Token} {pub : Publication} {binding : SnapshotBinding}
      (hNoReader : s.findFastLookup? readerId = none)
      (hSnap : s.findSnapshot? token.slot = some binding)
      (hSnapGen : binding.generation = token.generation)
      (hPub : s.findPublication? token.slot token.generation = some pub)
      (hAuth : token.session = s.registry.session)
      (hLive : pub.state = .live) :
      Step s (.beginFastObservation readerId token)
        { s with
            fastLookups := s.fastLookups ++
              [{ id := readerId, token := token, stage := .observed }] }

  | acquireTentativeLease
      {s : State}
      {readerId : Nat} {lookup : FastLookup}
      (hLookup : s.findFastLookup? readerId = some lookup)
      (hObserved : lookup.stage = .observed)
      (hNotSealed : s.leaseAdmission ≠ .sealed)
      (hNotClosed : s.registry.closed = false) :
      Step s (.acquireTentativeLease readerId)
        { s with fastLookups := s.updateFastLookupStage readerId .tentative }

  | abandonObservation
      {s : State}
      {readerId : Nat} {lookup : FastLookup}
      (hLookup : s.findFastLookup? readerId = some lookup)
      (hObserved : lookup.stage = .observed) :
      Step s (.abandonObservation readerId)
        { s with fastLookups := s.removeFastLookup readerId }

  | validateFastLookup
      {s : State} {reg' : Registry.State}
      {readerId : Nat} {lookup : FastLookup} {pub : Publication}
      (hLookup : s.findFastLookup? readerId = some lookup)
      (hTentative : lookup.stage = .tentative)
      (hPub : s.findPublication? lookup.token.slot lookup.token.generation = some pub)
      (hLive : pub.state = .live)
      (hReg : Registry.Step s.registry (.beginLookup lookup.token) reg') :
      Step s (.validateFastLookup readerId)
        { s with
            registry := reg'
            fastLookups := s.updateFastLookupStage readerId .validated }

  | rejectTentativeFastLookup
      {s : State}
      {readerId : Nat} {lookup : FastLookup} {pub : Publication}
      (hLookup : s.findFastLookup? readerId = some lookup)
      (hTentative : lookup.stage = .tentative)
      (hPub : s.findPublication? lookup.token.slot lookup.token.generation = some pub)
      (hNotLive : pub.state ≠ .live) :
      Step s (.rejectTentativeFastLookup readerId)
        { s with fastLookups := s.removeFastLookup readerId }

  | completeFastLookup
      {s : State} {reg' : Registry.State}
      {readerId : Nat} {lookup : FastLookup}
      (hLookup : s.findFastLookup? readerId = some lookup)
      (hValidated : lookup.stage = .validated)
      (hReg : Registry.Step s.registry .endLookup reg') :
      Step s (.completeFastLookup readerId)
        { s with
            registry := reg'
            fastLookups := s.removeFastLookup readerId }

  | fallbackFastLookup
      {s : State} {reg' : Registry.State}
      {readerId : Nat} {lookup : FastLookup} {pub : Publication}
      (hLookup : s.findFastLookup? readerId = some lookup)
      (hValidated : lookup.stage = .validated)
      (hPub : s.findPublication? lookup.token.slot lookup.token.generation = some pub)
      (hNotLive : pub.state ≠ .live)
      (hReg : Registry.Step s.registry .endLookup reg') :
      Step s (.fallbackFastLookup readerId)
        { s with
            registry := reg'
            fastLookups := s.removeFastLookup readerId }

  | beginSlowLookup
      {s : State} {reg' : Registry.State}
      {token : Token}
      (hNotSealed : s.leaseAdmission ≠ .sealed)
      (hReg : Registry.Step s.registry (.beginLookup token) reg') :
      Step s (.beginSlowLookup token)
        { s with registry := reg' }

  | endSlowLookup
      {s : State} {reg' : Registry.State}
      (hSlowLease : s.validatedFastLookups.length < s.registry.activeLeases)
      (hReg : Registry.Step s.registry .endLookup reg') :
      Step s .endSlowLookup
        { s with registry := reg' }

  | beginSealLeaseAdmission
      {s : State}
      (hOpen : s.leaseAdmission = .open) :
      Step s .beginSealLeaseAdmission
        { s with leaseAdmission := .sealing }

  | finishSealLeaseAdmission
      {s : State}
      (hSealing : s.leaseAdmission = .sealing) :
      Step s .finishSealLeaseAdmission
        { s with leaseAdmission := .sealed }

  | closeRegistry
      {s : State} {reg' : Registry.State}
      (hSealed : s.leaseAdmission = .sealed)
      (hReg : Registry.Step s.registry .closeRegistry reg') :
      Step s .closeRegistry
        { s with
            registry := reg'
            publications := s.updateClosingPublications
            snapshot := [] }

  | finishClose
      {s : State} {reg' : Registry.State}
      (hNoTentative : s.tentativeFastLookups = [])
      (hNoValidated : s.validatedFastLookups = [])
      (hReg : Registry.Step s.registry .finishClose reg') :
      Step s .finishClose s

inductive Reachable : State → State → Prop where
  | refl (s : State) : Reachable s s
  | tail {s t u : State} {e : Event} : Reachable s t → Step t e u → Reachable s u

end XlFnFormal.Handle.Registry.Snapshot
