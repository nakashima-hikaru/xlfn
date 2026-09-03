import XlFnFormal.TemporalReclamation.Transition

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.TemporalReclamation

theorem initial_invariant : initialState.Invariant := by
  dsimp [initialState, State.Invariant]
  refine ⟨?_, ?_, ?_, ?_, ?_⟩
  · intro hObs; contradiction
  · intro hPins; contradiction
  · omega
  · intro hRec; contradiction
  · intro _; exact ⟨rfl, rfl⟩

theorem step_preserves_invariant
    {s s' : State} {e : Event}
    (hInv : s.Invariant) (hStep : Step s e s') : s'.Invariant := by
  rcases hInv with ⟨hObsLive, hPinLive, hAdmBound, hRecDrained, hUnpubDrained⟩
  cases hStep with
  | publish hUnpub hNoPins hNoObs =>
      dsimp [State.Invariant]
      refine ⟨?_, ?_, ?_, ?_, ?_⟩
      · intro hObs
        rw [hNoObs] at hObs
        contradiction
      · intro hPins
        rw [hNoPins] at hPins
        contradiction
      · rw [hNoObs]; omega
      · intro hContra; contradiction
      · intro hContra; contradiction

  | enterLookup hNotRec =>
      dsimp [State.Invariant]
      refine ⟨hObsLive, hPinLive, by omega, ?_, ?_⟩
      · intro hRec
        contradiction
      · intro hUnpub
        exact hUnpubDrained hUnpub

  | observePointer hAdm hLive =>
      dsimp [State.Invariant]
      refine ⟨fun _ => hLive, hPinLive, by omega, ?_, ?_⟩
      · intro hContra
        rcases hLive with hPub | hRet
        · rw [hPub] at hContra; contradiction
        · rw [hRet] at hContra; contradiction
      · intro hContra
        rcases hLive with hPub | hRet
        · rw [hPub] at hContra; contradiction
        · rw [hRet] at hContra; contradiction

  | acquirePin hObs =>
      dsimp [State.Invariant]
      have hLive := hObsLive hObs
      refine ⟨?_, fun _ => hLive, by omega, ?_, ?_⟩
      · intro hObsGt
        exact hLive
      · intro hContra
        rcases hLive with hPub | hRet
        · rw [hPub] at hContra; contradiction
        · rw [hRet] at hContra; contradiction
      · intro hContra
        rcases hLive with hPub | hRet
        · rw [hPub] at hContra; contradiction
        · rw [hRet] at hContra; contradiction

  | leaveLookup hAdm =>
      dsimp [State.Invariant]
      refine ⟨hObsLive, hPinLive, by omega, ?_, ?_⟩
      · intro hRec
        have ⟨hAdm0, _, _⟩ := hRecDrained hRec
        omega
      · intro hUnpub
        exact hUnpubDrained hUnpub

  | unpin hPins =>
      dsimp [State.Invariant]
      refine ⟨hObsLive, ?_, hAdmBound, ?_, ?_⟩
      · intro hPinsGt
        exact hPinLive hPins
      · intro hRec
        have ⟨hAdm0, hObs0, hPins0⟩ := hRecDrained hRec
        omega
      · intro hUnpub
        have ⟨hObs0, hPins0⟩ := hUnpubDrained hUnpub
        omega

  | retire hPub =>
      dsimp [State.Invariant]
      refine ⟨?_, ?_, hAdmBound, ?_, ?_⟩
      · intro hObs
        exact Or.inr rfl
      · intro hPins
        exact Or.inr rfl
      · intro hContra; contradiction
      · intro hContra; contradiction

  | reclaim hRet hNoAdm hNoObs hNoPins =>
      dsimp [State.Invariant]
      refine ⟨?_, ?_, by omega, ?_, ?_⟩
      · intro hObs
        rw [hNoObs] at hObs
        contradiction
      · intro hPins
        rw [hNoPins] at hPins
        contradiction
      · intro _
        exact ⟨hNoAdm, hNoObs, hNoPins⟩
      · intro hContra; contradiction

theorem reachable_invariant
    {s : State} (hReach : Reachable initialState s) : s.Invariant := by
  induction hReach with
  | refl => exact initial_invariant
  | tail hR hStep ih => exact step_preserves_invariant ih hStep

end XlFnFormal.TemporalReclamation
