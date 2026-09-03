import XlFnFormal.Handle.Publication.Safety
import XlFnFormal.TemporalOwnership.Safety

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Publication

open XlFnFormal.TemporalOwnership

/-! Refinement theorem mapping each `Publication` to the generic `TemporalOwnership.State`. -/

def toTemporal (pub : Publication) : TemporalOwnership.State :=
  { ownerPresent := pub.owned
  , published    := pub.published
  , gate         := pub.gate
  , readers      := pub.admitted }

theorem enter_refinement
    (pub : Publication)
    (hGate : pub.gate = .open)
    (hPub : pub.published = true)
    (hOwned : pub.owned = true) :
    TemporalOwnership.Step (toTemporal pub) .enter
      (toTemporal { pub with admitted := pub.admitted + 1 }) := by
  dsimp [toTemporal]
  exact TemporalOwnership.Step.enter hOwned hPub hGate

theorem release_refinement
    (pub : Publication)
    (hAdm : pub.admitted > 0) :
    TemporalOwnership.Step (toTemporal pub) .release
      (toTemporal { pub with admitted := pub.admitted - 1 }) := by
  dsimp [toTemporal]
  exact TemporalOwnership.Step.release hAdm

theorem reclaim_refinement
    (pub : Publication)
    (hNotPub : pub.published = false)
    (hSealed : pub.gate = .sealed)
    (hDrained : pub.admitted = 0)
    (hOwned : pub.owned = true) :
    TemporalOwnership.Step (toTemporal pub) .reclaim
      (toTemporal { pub with owned := false }) := by
  dsimp [toTemporal]
  exact TemporalOwnership.Step.reclaim hOwned hSealed hNotPub hDrained

end XlFnFormal.Handle.Publication
