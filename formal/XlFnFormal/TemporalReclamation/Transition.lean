import XlFnFormal.TemporalReclamation.Model

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.TemporalReclamation

inductive Event where
  | publish
  | enterLookup
  | observePointer
  | acquirePin
  | leaveLookup
  | unpin
  | retire
  | reclaim
deriving DecidableEq, Repr

inductive Step : State → Event → State → Prop where
  /-- TR-PUBLISH: Object is published to readers. -/
  | publish
      {s : State}
      (hUnpub : s.status = .unpublished)
      (hNoPins : s.pins = 0)
      (hNoObs : s.observing = 0) :
      Step s .publish { s with status := .published }

  /-- TR-LOOKUP-ENTER: Reader acquires short lookup admission permit. -/
  | enterLookup
      {s : State}
      (hNotRec : s.status ≠ .reclaimed) :
      Step s .enterLookup { s with admissions := s.admissions + 1 }

  /-- TR-OBSERVE-POINTER: Reader holding admission observes the pointer.
      Requires object to be published or retired (cannot observe an unpublished or reclaimed pointer). -/
  | observePointer
      {s : State}
      (hAdm : s.observing < s.admissions)
      (hLive : s.status = .published ∨ s.status = .retired) :
      Step s .observePointer { s with observing := s.observing + 1 }

  /-- TR-ACQUIRE-PIN: Reader with active observation acquires a long-lived pin. -/
  | acquirePin
      {s : State}
      (hObs : s.observing > 0) :
      Step s .acquirePin { s with observing := s.observing - 1, pins := s.pins + 1 }

  /-- TR-LOOKUP-LEAVE: Reader releases lookup admission permit after acquiring pin or missing. -/
  | leaveLookup
      {s : State}
      (hAdm : s.observing < s.admissions) :
      Step s .leaveLookup { s with admissions := s.admissions - 1 }

  /-- TR-UNPIN: Active lease or reference drops its pin. -/
  | unpin
      {s : State}
      (hPins : s.pins > 0) :
      Step s .unpin { s with pins := s.pins - 1 }

  /-- TR-RETIRE: Object is retired from publication (e.g. evicted or removed). -/
  | retire
      {s : State}
      (hPub : s.status = .published) :
      Step s .retire { s with status := .retired }

  /-- TR-RECLAIM: Object memory is reclaimed.
      Requires object to be retired, admissions drained to 0, no in-flight observers, and no pins. -/
  | reclaim
      {s : State}
      (hRet : s.status = .retired)
      (hNoAdm : s.admissions = 0)
      (hNoObs : s.observing = 0)
      (hNoPins : s.pins = 0) :
      Step s .reclaim { s with status := .reclaimed }

inductive Reachable : State → State → Prop where
  | refl (s : State) : Reachable s s
  | tail {s t u : State} {e : Event} :
      Reachable s t → Step t e u → Reachable s u

end XlFnFormal.TemporalReclamation
