import XlFnFormal.TemporalOwnership.Model

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.TemporalOwnership

inductive Event where
  | publish
  | enter
  | release
  | «seal»
  | withdraw
  | reclaim
  | reopen
deriving DecidableEq, Repr

inductive Step : State → Event → State → Prop where
  | publish
      {s : State}
      (hOwner : s.ownerPresent = true)
      (hNotPub : s.published = false)
      (hOpen : s.gate = .open) :
      Step s .publish { s with published := true }

  | enter
      {s : State}
      (hOwner : s.ownerPresent = true)
      (hPub : s.published = true)
      (hOpen : s.gate = .open) :
      Step s .enter { s with readers := s.readers + 1 }

  | release
      {s : State}
      (hReaders : s.readers > 0) :
      Step s .release { s with readers := s.readers - 1 }

  | «seal»
      {s : State}
      (hOpen : s.gate = .open) :
      Step s .«seal» { s with gate := .sealed }

  | withdraw
      {s : State}
      (hPub : s.published = true) :
      Step s .withdraw { s with published := false }

  | reclaim
      {s : State}
      (hOwner : s.ownerPresent = true)
      (hSealed : s.gate = .sealed)
      (hNotPub : s.published = false)
      (hDrained : s.readers = 0) :
      Step s .reclaim { s with ownerPresent := false }

  | reopen
      {s : State}
      (hNotOwner : s.ownerPresent = false)
      (hSealed : s.gate = .sealed)
      (hNotPub : s.published = false)
      (hDrained : s.readers = 0) :
      Step s .reopen { s with ownerPresent := true, gate := .open }

inductive Reachable : State → State → Prop where
  | refl (s : State) : Reachable s s
  | tail {s t u : State} {e : Event} :
      Reachable s t → Step t e u → Reachable s u

end XlFnFormal.TemporalOwnership
