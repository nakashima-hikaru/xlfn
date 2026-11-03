import XlFnFormal.TemporalReclamation.Invariant

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.TemporalReclamation

/-- TR-OBSERVE-SAFETY: Any active observer holds a live (non-reclaimed) reference. -/
theorem observingImpliesLive
    {s : State} (hInv : s.Invariant) (hObs : s.observing > 0) :
    s.status = .published ∨ s.status = .retired :=
  hInv.1 hObs

/-- TR-LEASE-SAFETY: Any active pin holds a live (non-reclaimed) reference. -/
theorem pinImpliesLive
    {s : State} (hInv : s.Invariant) (hPins : s.pins > 0) :
    s.status = .published ∨ s.status = .retired :=
  hInv.2.1 hPins

/-- TR-RECLAIM-DRAINED: A reclaimed object has zero active admissions, observers, and pins. -/
theorem reclaimedImpliesNoCapabilities
    {s : State} (hInv : s.Invariant) (hRec : s.status = .reclaimed) :
    s.admissions = 0 ∧ s.observing = 0 ∧ s.pins = 0 :=
  hInv.2.2.2.1 hRec

/-- TR-RECLAIM-REQUIRES: Reclaim transition requires retired state and zero capabilities. -/
theorem reclaimRequiresDrained
    {s s' : State} (hStep : Step s .reclaim s') :
    s.status = .retired ∧ s.admissions = 0 ∧ s.observing = 0 ∧ s.pins = 0 := by
  cases hStep with
  | reclaim hRet hNoAdm hNoObs hNoPins =>
      exact ⟨hRet, hNoAdm, hNoObs, hNoPins⟩

/-- TR-OBSERVATION-PRECLUDES-RECLAIM: Pointer observation strictly precludes concurrent reclaim. -/
theorem observationPrecludesReclaim
    {s : State} (hObs : s.observing > 0) :
    ¬ ∃ s', Step s .reclaim s' := by
  intro ⟨s', hStep⟩
  cases hStep with
  | reclaim _ _ hNoObs _ =>
      rw [hNoObs] at hObs
      contradiction

/-- TR-ADMISSION-PRECLUDES-RECLAIM: Active admission domain strictly precludes reclaim. -/
theorem admissionPrecludesReclaim
    {s : State} (hAdm : s.admissions > 0) :
    ¬ ∃ s', Step s .reclaim s' := by
  intro ⟨s', hStep⟩
  cases hStep with
  | reclaim _ hNoAdm _ _ =>
      rw [hNoAdm] at hAdm
      contradiction

/-- TR-PIN-PRECLUDES-RECLAIM: Active pin strictly precludes reclaim. -/
theorem pinPrecludesReclaim
    {s : State} (hPins : s.pins > 0) :
    ¬ ∃ s', Step s .reclaim s' := by
  intro ⟨s', hStep⟩
  cases hStep with
  | reclaim _ _ _ hNoPins =>
      rw [hNoPins] at hPins
      contradiction

/-- Fundamental Temporal Safety: In any reachable state, holding a capability
    (either observing a pointer or holding a pin) guarantees the object is not reclaimed. -/
theorem noUseAfterReclaim
    {s : State} (hReach : Reachable initialState s)
    (hCap : s.observing > 0 ∨ s.pins > 0) :
    s.status ≠ .reclaimed := by
  have hInv := reachable_invariant hReach
  intro hContra
  rcases hCap with hObs | hPins
  · have hLive := observingImpliesLive hInv hObs
    rcases hLive with hPub | hRet
    · rw [hContra] at hPub; contradiction
    · rw [hContra] at hRet; contradiction
  · have hLive := pinImpliesLive hInv hPins
    rcases hLive with hPub | hRet
    · rw [hContra] at hPub; contradiction
    · rw [hContra] at hRet; contradiction

/-- Sequence Invariance: It is impossible to execute `reclaim` between `observePointer` and `acquirePin`. -/
theorem noReclaimDuringObservation
    {s : State} (hReach : Reachable initialState s) (hObs : s.observing > 0) :
    ¬ ∃ s', Step s .reclaim s' :=
  observationPrecludesReclaim hObs

end XlFnFormal.TemporalReclamation
