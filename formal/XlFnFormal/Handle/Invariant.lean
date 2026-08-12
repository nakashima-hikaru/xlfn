import XlFnFormal.Handle.Transition

set_option autoImplicit false

namespace XlFnFormal.Handle

def PhaseInvariant (s : State) : Prop :=
  match s.phase with
  | .«open» => True
  | .drainingPrepares => True
  | .registryClosed => State.NoLiveSlots s.slots ∧ s.activePrepares = 0 ∧ s.activeInitializers = 0
  | .closed => State.NoLiveSlots s.slots ∧ s.activePrepares = 0 ∧ s.activeInitializers = 0 ∧ s.activeLeases = 0

def OperationInvariant (s : State) : Prop :=
  s.activeInitializers ≤ s.activePrepares

def Invariant (s : State) : Prop :=
  PhaseInvariant s ∧ OperationInvariant s

theorem map_closeSlot_noLiveSlots (slots : List SlotState) :
    State.NoLiveSlots (slots.map closeSlot) := by
  intro slot hMem
  rcases List.mem_map.mp hMem with ⟨orig, _, rfl⟩
  cases orig
  · dsimp [closeSlot]
    split <;> exact trivial
  · dsimp [closeSlot]
    split <;> exact trivial
  · dsimp [closeSlot]
    exact trivial

theorem noLiveSlots_contradiction {s : State} {i : SlotId} {hIn : i < s.slots.length} {g : Generation}
    (hNoLive : State.NoLiveSlots s.slots)
    (hLive : s.slots.get ⟨i, hIn⟩ = .live g) : False := by
  have hMem : s.slots.get ⟨i, hIn⟩ ∈ s.slots := List.get_mem s.slots ⟨i, hIn⟩
  have hNo := hNoLive (s.slots.get ⟨i, hIn⟩) hMem
  rw [hLive] at hNo
  exact hNo

theorem Step.operationInvariant_preserved {s s' : State} {e : Event} (hStep : Step s e s') (hInv : OperationInvariant s) :
    OperationInvariant s' := by
  cases hStep with
  | beginPrepare =>
      dsimp [OperationInvariant] at *
      exact Nat.le_add_right_of_le hInv
  | endPrepare hPrep =>
      dsimp [OperationInvariant] at *
      exact Nat.le_sub_one_of_lt hPrep
  | beginInitialize hPhase hPrep =>
      dsimp [OperationInvariant] at *
      exact hPrep
  | finishInitialize hInit =>
      dsimp [OperationInvariant] at *
      exact Nat.le_trans (Nat.sub_le s.activeInitializers 1) hInv
  | publishTopic => exact hInv
  | rollbackPendingReuse => exact hInv
  | rollbackPendingRetire => exact hInv
  | insertFresh => exact hInv
  | insertReuse => exact hInv
  | removeReuse => exact hInv
  | removeRetire => exact hInv
  | beginLookup => exact hInv
  | endLookup => exact hInv
  | sealTopics => exact hInv
  | closeRegistry => exact hInv
  | finishClose => exact hInv

theorem Step.phaseInvariant_preserved {s s' : State} {e : Event} (hStep : Step s e s') (hInv : PhaseInvariant s) :
    PhaseInvariant s' := by
  cases hStep with
  | beginPrepare hPhase =>
      dsimp [PhaseInvariant]
      cases hPhase with
      | inl hO => rw [hO]; dsimp
      | inr hDP => rw [hDP]; dsimp
  | endPrepare hPrep =>
      dsimp [PhaseInvariant]
      cases hP : s.phase with
      | «open» => exact trivial
      | drainingPrepares => exact trivial
      | registryClosed =>
          have hInv' : State.NoLiveSlots s.slots ∧ s.activePrepares = 0 ∧ s.activeInitializers = 0 := by
            unfold PhaseInvariant at hInv; rw [hP] at hInv; exact hInv
          rw [hInv'.2.1] at hPrep; contradiction
      | closed =>
          have hInv' : State.NoLiveSlots s.slots ∧ s.activePrepares = 0 ∧ s.activeInitializers = 0 ∧ s.activeLeases = 0 := by
            unfold PhaseInvariant at hInv; rw [hP] at hInv; exact hInv
          rw [hInv'.2.1] at hPrep; contradiction
  | beginInitialize hPhase hPrep =>
      dsimp [PhaseInvariant]
      rw [hPhase]
      dsimp
  | finishInitialize hInit =>
      dsimp [PhaseInvariant]
      cases hP : s.phase with
      | «open» => exact trivial
      | drainingPrepares => exact trivial
      | registryClosed =>
          have hInv' : State.NoLiveSlots s.slots ∧ s.activePrepares = 0 ∧ s.activeInitializers = 0 := by
            unfold PhaseInvariant at hInv; rw [hP] at hInv; exact hInv
          rw [hInv'.2.2] at hInit; contradiction
      | closed =>
          have hInv' : State.NoLiveSlots s.slots ∧ s.activePrepares = 0 ∧ s.activeInitializers = 0 ∧ s.activeLeases = 0 := by
            unfold PhaseInvariant at hInv; rw [hP] at hInv; exact hInv
          rw [hInv'.2.2.1] at hInit; contradiction
  | publishTopic hPhase hInit =>
      dsimp [PhaseInvariant]
      rw [hPhase]
      dsimp
  | rollbackPendingReuse hInit hAuth hInBounds hLive hNextGen =>
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
  | rollbackPendingRetire hInit hAuth hInBounds hLive hExhausted =>
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
  | insertFresh hMay =>
      dsimp [PhaseInvariant]
      cases hMay with
      | inl hO => rw [hO]; dsimp
      | inr hDP => rw [hDP.1]; dsimp
  | insertReuse hMay hInBounds hVacant =>
      dsimp [PhaseInvariant]
      cases hMay with
      | inl hO => rw [hO]; dsimp
      | inr hDP => rw [hDP.1]; dsimp
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
  | removeRetire hAuth hInBounds hLive hExhausted =>
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
          have hInv' : State.NoLiveSlots s.slots ∧ s.activePrepares = 0 ∧ s.activeInitializers = 0 := by
            unfold PhaseInvariant at hInv; rw [hP] at hInv; exact hInv
          exact hInv'
      | closed =>
          have hInv' : State.NoLiveSlots s.slots ∧ s.activePrepares = 0 ∧ s.activeInitializers = 0 ∧ s.activeLeases = 0 := by
            unfold PhaseInvariant at hInv; rw [hP] at hInv; exact hInv
          rw [hInv'.2.2.2] at hLease; contradiction
  | sealTopics hPhase =>
      dsimp [PhaseInvariant]
  | closeRegistry hPhase hNoInits hNoPrepares =>
      dsimp [PhaseInvariant]
      exact ⟨map_closeSlot_noLiveSlots s.slots, hNoPrepares, hNoInits⟩
  | finishClose hPhase hNoLeases =>
      dsimp [PhaseInvariant]
      have hInv' : State.NoLiveSlots s.slots ∧ s.activePrepares = 0 ∧ s.activeInitializers = 0 := by
        unfold PhaseInvariant at hInv; rw [hPhase] at hInv; exact hInv
      exact ⟨hInv'.1, hInv'.2.1, hInv'.2.2, hNoLeases⟩

theorem Step.invariant_preserved {s s' : State} {e : Event} (hStep : Step s e s') (hInv : Invariant s) :
    Invariant s' := by
  exact ⟨Step.phaseInvariant_preserved hStep hInv.1, Step.operationInvariant_preserved hStep hInv.2⟩

theorem reachable_phaseInvariant {init s : State} (hReach : Reachable init s) (hInit : PhaseInvariant init) :
    PhaseInvariant s := by
  induction hReach with
  | init => exact hInit
  | step _ hStep ih => exact Step.phaseInvariant_preserved hStep ih

theorem reachable_operationInvariant {init s : State} (hReach : Reachable init s) (hInit : OperationInvariant init) :
    OperationInvariant s := by
  induction hReach with
  | init => exact hInit
  | step _ hStep ih => exact Step.operationInvariant_preserved hStep ih

theorem reachable_invariant {init s : State} (hReach : Reachable init s) (hInit : Invariant init) :
    Invariant s := by
  exact ⟨reachable_phaseInvariant hReach hInit.1, reachable_operationInvariant hReach hInit.2⟩

end XlFnFormal.Handle
