import XlFnFormal.TemporalOwnership.Transition

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.TemporalOwnership

theorem initial_invariant : initialState.Invariant := by
  dsimp [initialState, State.Invariant]
  refine ⟨?_, ?_, ?_⟩
  · intro hPub; contradiction
  · intro hReaders; contradiction
  · intro hNotOwner; contradiction

theorem step_preserves_invariant
    {s s' : State} {e : Event}
    (hInv : s.Invariant) (hStep : Step s e s') : s'.Invariant := by
  rcases hInv with ⟨hPubOwn, hReadOwn, hNotOwn⟩
  cases hStep with
  | publish hOwner hNotPub hOpen =>
      dsimp [State.Invariant]
      refine ⟨fun _ => hOwner, fun _ => hOwner, ?_⟩
      intro hContra
      rw [hOwner] at hContra
      contradiction
  | enter hOwner hPub hOpen =>
      dsimp [State.Invariant]
      refine ⟨fun _ => hOwner, fun _ => hOwner, ?_⟩
      intro hContra
      rw [hOwner] at hContra
      contradiction
  | release hReaders =>
      dsimp [State.Invariant]
      refine ⟨?_, ?_, ?_⟩
      · intro hPub
        exact hPubOwn hPub
      · intro hGt
        apply hReadOwn
        exact Nat.lt_of_lt_of_le (Nat.zero_lt_one) (Nat.le_trans (Nat.succ_le_of_lt hGt) (by omega))
      · intro hNotOwner
        have ⟨_, hZero⟩ := hNotOwn hNotOwner
        omega
  | «seal» hOpen =>
      dsimp [State.Invariant]
      exact ⟨hPubOwn, hReadOwn, hNotOwn⟩
  | withdraw hPub =>
      dsimp [State.Invariant]
      refine ⟨?_, hReadOwn, ?_⟩
      · intro hContra; contradiction
      · intro hNotOwner
        have ⟨_, hZero⟩ := hNotOwn hNotOwner
        exact ⟨rfl, hZero⟩
  | reclaim hOwner hSealed hNotPub hDrained =>
      dsimp [State.Invariant]
      refine ⟨?_, ?_, ?_⟩
      · intro hContra
        rw [hNotPub] at hContra
        contradiction
      · intro hContra
        rw [hDrained] at hContra
        contradiction
      · intro _
        exact ⟨hNotPub, hDrained⟩
  | reopen hNotOwner hSealed hNotPub hDrained =>
      dsimp [State.Invariant]
      refine ⟨fun _ => rfl, fun _ => rfl, ?_⟩
      intro hContra
      contradiction

theorem reachable_invariant
    {s : State} (hReach : Reachable initialState s) : s.Invariant := by
  induction hReach with
  | refl => exact initial_invariant
  | tail hR hStep ih => exact step_preserves_invariant ih hStep

end XlFnFormal.TemporalOwnership
