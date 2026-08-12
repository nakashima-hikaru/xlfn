import XlFnFormal.Handle.Transition

set_option autoImplicit false

namespace XlFnFormal.Handle

def PhaseInvariant (s : State) : Prop :=
  match s.phase with
  | .«open» => True
  | .drainingPrepares => True
  | .registryClosed => State.NoLiveSlots s.slots ∧ s.activePrepares = 0
  | .closed => State.NoLiveSlots s.slots ∧ s.activePrepares = 0 ∧ s.activeLeases = 0

theorem map_closeRegistry_noLiveSlots (slots : List SlotState) :
    State.NoLiveSlots (slots.map (fun slot => match slot with | .live g => SlotState.vacant (g + 1) | other => other)) := by
  intro slot hMem
  rcases List.mem_map.mp hMem with ⟨orig, _, rfl⟩
  cases orig <;> simp [State.SlotNoLive]

theorem noLiveSlots_contradiction {s : State} {i : SlotId} {hIn : i < s.slots.length} {g : Generation}
    (hNoLive : State.NoLiveSlots s.slots)
    (hLive : s.slots.get ⟨i, hIn⟩ = .live g) : False := by
  have hMem : s.slots.get ⟨i, hIn⟩ ∈ s.slots := List.get_mem s.slots ⟨i, hIn⟩
  have hNo := hNoLive (s.slots.get ⟨i, hIn⟩) hMem
  rw [hLive] at hNo
  exact hNo

theorem Step.phaseInvariant_preserved {s s' : State} {e : Event} (hStep : Step s e s') (hInv : PhaseInvariant s) :
    PhaseInvariant s' := by
  cases hStep with
  | beginPrepare hPhase =>
      dsimp [PhaseInvariant]
      rw [hPhase]
      dsimp
  | endPrepare hPrep =>
      dsimp [PhaseInvariant]
      cases hP : s.phase with
      | «open» => exact trivial
      | drainingPrepares => exact trivial
      | registryClosed =>
          have hInv' : State.NoLiveSlots s.slots ∧ s.activePrepares = 0 := by
            unfold PhaseInvariant at hInv; rw [hP] at hInv; exact hInv
          rw [hInv'.2] at hPrep; contradiction
      | closed =>
          have hInv' : State.NoLiveSlots s.slots ∧ s.activePrepares = 0 ∧ s.activeLeases = 0 := by
            unfold PhaseInvariant at hInv; rw [hP] at hInv; exact hInv
          rw [hInv'.2.1] at hPrep; contradiction
  | insert hPhase hInBounds hVacant =>
      dsimp [PhaseInvariant]
      cases hPhase with
      | inl hO => rw [hO]; dsimp
      | inr hDP => rw [hDP]; dsimp
  | removeReuse hAuth hInBounds hLive hNextGen =>
      dsimp [PhaseInvariant]
      cases hP : s.phase with
      | «open» => exact trivial
      | drainingPrepares => exact trivial
      | registryClosed =>
          have hNoLive : State.NoLiveSlots s.slots := by
            unfold PhaseInvariant at hInv; rw [hP] at hInv; exact hInv.1
          exfalso; exact noLiveSlots_contradiction hNoLive hLive
      | closed =>
          have hNoLive : State.NoLiveSlots s.slots := by
            unfold PhaseInvariant at hInv; rw [hP] at hInv; exact hInv.1
          exfalso; exact noLiveSlots_contradiction hNoLive hLive
  | removeRetire hAuth hInBounds hLive =>
      dsimp [PhaseInvariant]
      cases hP : s.phase with
      | «open» => exact trivial
      | drainingPrepares => exact trivial
      | registryClosed =>
          have hNoLive : State.NoLiveSlots s.slots := by
            unfold PhaseInvariant at hInv; rw [hP] at hInv; exact hInv.1
          exfalso; exact noLiveSlots_contradiction hNoLive hLive
      | closed =>
          have hNoLive : State.NoLiveSlots s.slots := by
            unfold PhaseInvariant at hInv; rw [hP] at hInv; exact hInv.1
          exfalso; exact noLiveSlots_contradiction hNoLive hLive
  | beginLookup hPhase hAuth hInBounds hLive =>
      dsimp [PhaseInvariant]
      cases hP : s.phase with
      | «open» => exact trivial
      | drainingPrepares => exact trivial
      | registryClosed =>
          have hNoLive : State.NoLiveSlots s.slots := by
            unfold PhaseInvariant at hInv; rw [hP] at hInv; exact hInv.1
          exfalso; exact noLiveSlots_contradiction hNoLive hLive
      | closed => rw [hP] at hPhase; contradiction
  | endLookup hLease =>
      dsimp [PhaseInvariant]
      cases hP : s.phase with
      | «open» => exact trivial
      | drainingPrepares => exact trivial
      | registryClosed =>
          have hInv' : State.NoLiveSlots s.slots ∧ s.activePrepares = 0 := by
            unfold PhaseInvariant at hInv; rw [hP] at hInv; exact hInv
          exact hInv'
      | closed =>
          have hInv' : State.NoLiveSlots s.slots ∧ s.activePrepares = 0 ∧ s.activeLeases = 0 := by
            unfold PhaseInvariant at hInv; rw [hP] at hInv; exact hInv
          rw [hInv'.2.2] at hLease; contradiction
  | sealTopics hPhase =>
      dsimp [PhaseInvariant]
  | closeRegistry hPhase hNoPrepares =>
      dsimp [PhaseInvariant]
      exact ⟨map_closeRegistry_noLiveSlots s.slots, hNoPrepares⟩
  | finishClose hPhase hNoLeases =>
      dsimp [PhaseInvariant]
      have hInv' : State.NoLiveSlots s.slots ∧ s.activePrepares = 0 := by
        unfold PhaseInvariant at hInv; rw [hPhase] at hInv; exact hInv
      exact ⟨hInv'.1, hInv'.2, hNoLeases⟩

theorem reachable_phaseInvariant {init s : State} (hReach : Reachable init s) (hInit : PhaseInvariant init) :
    PhaseInvariant s := by
  induction hReach with
  | init => exact hInit
  | step _ hStep ih => exact Step.phaseInvariant_preserved hStep ih

end XlFnFormal.Handle
