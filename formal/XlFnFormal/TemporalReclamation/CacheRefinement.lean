import XlFnFormal.TemporalReclamation.Safety

/-! # Cache Refinement for Temporal Reclamation

    Formal refinement theorem mapping the `CalculationCache` per-node pin and
    lookup admission lifecycle onto the generic `TemporalReclamation` protocol.

    ## Subsystem Protocol Architecture Mapping:
    | Subsystem    | Admission / Domain Gate | Pin Capability | Retirement Point    | Reclamation Point   |
    |--------------|-------------------------|----------------|---------------------|---------------------|
    | Cache        | CacheLookupDomain permit| CacheLease pin | Moka eviction       | CacheNode drop      |
    | Handle       | HandleReadDomain permit | HandleLease    | Binding removal     | ObjectArena remove  |
    | RTD Callback | ServerOperationBarrier  | (immediate)    | Callback replace    | Callback Box drop   |
    | Async        | Generation admission    | Active task    | Generation rollover | State Box drop      |
-/

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.TemporalReclamation.Cache

open XlFnFormal.TemporalReclamation

/-- Abstract state of a cache node within the CalculationCache. -/
structure CacheAbstractState where
  resident   : Bool
  reclaimed  : Bool
  admissions : Nat
  observing  : Nat
  pins       : Nat
deriving DecidableEq, Repr

/-- Mapping from the concrete Cache state to generic TemporalReclamation.State. -/
def cacheToTemporal (c : CacheAbstractState) : TemporalReclamation.State :=
  { status :=
      if c.reclaimed then .reclaimed
      else if c.resident then .published
      else .retired
  , admissions := c.admissions
  , observing  := c.observing
  , pins       := c.pins }

/-- TR-LOOKUP-ENTER refines into TemporalReclamation.Step.enterLookup. -/
theorem cacheLookupEnterRefines
    (c : CacheAbstractState)
    (hNotRec : c.reclaimed = false) :
    Step (cacheToTemporal c) .enterLookup
      (cacheToTemporal { c with admissions := c.admissions + 1 }) := by
  have hNotRecStatus : (cacheToTemporal c).status ≠ .reclaimed := by
    dsimp [cacheToTemporal]
    rw [hNotRec]
    dsimp
    split <;> intro h <;> contradiction
  exact Step.enterLookup hNotRecStatus

/-- TR-OBSERVE-POINTER refines into TemporalReclamation.Step.observePointer. -/
theorem cacheObservePointerRefines
    (c : CacheAbstractState)
    (hNotRec : c.reclaimed = false)
    (hAdm : c.observing < c.admissions) :
    Step (cacheToTemporal c) .observePointer
      (cacheToTemporal { c with observing := c.observing + 1 }) := by
  have hLive : (cacheToTemporal c).status = .published ∨
               (cacheToTemporal c).status = .retired := by
    dsimp [cacheToTemporal]
    rw [hNotRec]
    dsimp
    by_cases hRes : c.resident
    · rw [if_pos hRes]
      exact Or.inl rfl
    · rw [if_neg hRes]
      exact Or.inr rfl
  exact Step.observePointer hAdm hLive

/-- TR-ACQUIRE-PIN refines into TemporalReclamation.Step.acquirePin. -/
theorem cacheAcquirePinRefines
    (c : CacheAbstractState)
    (hObs : c.observing > 0) :
    Step (cacheToTemporal c) .acquirePin
      (cacheToTemporal { c with observing := c.observing - 1, pins := c.pins + 1 }) := by
  dsimp [cacheToTemporal]
  exact Step.acquirePin hObs

/-- TR-LOOKUP-LEAVE refines into TemporalReclamation.Step.leaveLookup. -/
theorem cacheLookupLeaveRefines
    (c : CacheAbstractState)
    (hAdm : c.observing < c.admissions) :
    Step (cacheToTemporal c) .leaveLookup
      (cacheToTemporal { c with admissions := c.admissions - 1 }) := by
  dsimp [cacheToTemporal]
  exact Step.leaveLookup hAdm

/-- TR-UNPIN refines into TemporalReclamation.Step.unpin. -/
theorem cacheUnpinRefines
    (c : CacheAbstractState)
    (hPins : c.pins > 0) :
    Step (cacheToTemporal c) .unpin
      (cacheToTemporal { c with pins := c.pins - 1 }) := by
  dsimp [cacheToTemporal]
  exact Step.unpin hPins

/-- TR-RETIRE refines into TemporalReclamation.Step.retire. -/
theorem cacheEvictionRefines
    (c : CacheAbstractState)
    (hPub : c.resident = true)
    (hNotRec : c.reclaimed = false) :
    Step (cacheToTemporal c) .retire
      (cacheToTemporal { c with resident := false }) := by
  have hStatusPub : (cacheToTemporal c).status = .published := by
    dsimp [cacheToTemporal]
    rw [hNotRec, hPub]
    rfl
  have hStep := Step.retire hStatusPub
  have hEq : (cacheToTemporal { c with resident := false }) =
             { (cacheToTemporal c) with status := .retired } := by
    dsimp [cacheToTemporal]
    rw [hNotRec]
    rfl
  rw [hEq]
  exact hStep

/-- TR-RECLAIM refines into TemporalReclamation.Step.reclaim. -/
theorem cacheReclaimRefines
    (c : CacheAbstractState)
    (hRet : c.resident = false)
    (hNotRec : c.reclaimed = false)
    (hNoAdm : c.admissions = 0)
    (hNoObs : c.observing = 0)
    (hNoPins : c.pins = 0) :
    Step (cacheToTemporal c) .reclaim
      (cacheToTemporal { c with reclaimed := true }) := by
  have hStatusRet : (cacheToTemporal c).status = .retired := by
    dsimp [cacheToTemporal]
    rw [hNotRec, hRet]
    rfl
  have hStep := Step.reclaim hStatusRet hNoAdm hNoObs hNoPins
  exact hStep

/-- Safety Refinement: A reachable cache node with an active observation or pin
    capability is guaranteed never to be in a reclaimed state (no use-after-reclaim). -/
theorem cacheSafetyRefinement
    {c : CacheAbstractState}
    (hReach : Reachable initialState (cacheToTemporal c))
    (hActive : c.observing > 0 ∨ c.pins > 0) :
    c.reclaimed = false := by
  have hNotRec := noUseAfterReclaim hReach hActive
  dsimp [cacheToTemporal] at hNotRec
  cases hRec : c.reclaimed with
  | true =>
      rw [hRec] at hNotRec
      dsimp at hNotRec
      exact False.elim (hNotRec rfl)
  | false =>
      rfl

end XlFnFormal.TemporalReclamation.Cache
