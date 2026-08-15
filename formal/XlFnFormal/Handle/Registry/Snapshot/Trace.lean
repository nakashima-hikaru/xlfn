import XlFnFormal.Handle.Registry.Snapshot.Safety
import XlFnFormal.Handle.Registry.Snapshot.Checker

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Registry.Snapshot

open XlFnFormal.Handle.Registry

theorem fast_lookup_success_trace
    (session : SessionId) :
    let s0 := initialState session
    let token : Token := { session := session, slot := 0, generation := 1 }
    let s1 := { s0 with
      registry := { s0.registry with slots := [SlotState.live 1] },
      publications := [{ slot := 0, generation := 1, state := .live }],
      snapshot := [{ slot := 0, generation := 1 }] }
    let s2 := { s1 with
      registry := { s1.registry with activeLeases := 1 },
      fastLookups := [{ id := 10, token := token }] }
    let s3 := { s2 with
      registry := { s2.registry with activeLeases := 0 },
      fastLookups := [] }
    let s4 := { s3 with
      registry := { s3.registry with
        closed := true,
        slots := [SlotState.vacant 2] },
      publications := [{ slot := 0, generation := 1, state := .closing }],
      snapshot := [] }
    Step s0 .insertFresh s1 ∧
    Step s1 (.beginFastLookup 10 token) s2 ∧
    Step s2 (.completeFastLookup 10) s3 ∧
    Step s3 .closeRegistry s4 ∧
    Step s4 .finishClose s4 ∧
    CloseCertified s4.registry := by
  intro s0 token s1 s2 s3 s4
  have hReg1 : Registry.Step s0.registry .insertFresh s1.registry := Registry.Step.insertFresh (by rfl)
  have hStep1 : Step s0 .insertFresh s1 := Step.insertFresh hReg1 (by rfl) (by rfl)
  have hReg2 : Registry.Step s1.registry (.beginLookup token) s2.registry :=
    Registry.Step.beginLookup (by rfl) (by rfl) Nat.zero_lt_one (by rfl)
  have hStep2 : Step s1 (.beginFastLookup 10 token) s2 :=
    Step.beginFastLookup (by rfl) (by rfl) (by rfl) (by rfl) (by rfl) hReg2
  have hReg3 : Registry.Step s2.registry .endLookup s3.registry :=
    Registry.Step.endLookup Nat.zero_lt_one
  have hStep3 : Step s2 (.completeFastLookup 10) s3 :=
    Step.completeFastLookup (by rfl) hReg3
  have hReg4 : Registry.Step s3.registry .closeRegistry s4.registry :=
    Registry.Step.closeRegistry (by rfl)
  have hStep4 : Step s3 .closeRegistry s4 := Step.closeRegistry hReg4
  have hReg5 : Registry.Step s4.registry .finishClose s4.registry :=
    Registry.Step.finishClose (by rfl) (by rfl)
  have hStep5 : Step s4 .finishClose s4 := Step.finishClose hReg5
  have hReach1 : Reachable s0 s1 := Reachable.tail (Reachable.refl s0) hStep1
  have hReach2 : Reachable s0 s2 := Reachable.tail hReach1 hStep2
  have hReach3 : Reachable s0 s3 := Reachable.tail hReach2 hStep3
  have hReach4 : Reachable s0 s4 := Reachable.tail hReach3 hStep4
  have hCert := close_certified_when_finished hReach4 hStep5
  exact ⟨hStep1, hStep2, hStep3, hStep4, hStep5, hCert.1⟩

theorem fast_lookup_race_remove_linearization_trace
    (session : SessionId) :
    let s0 := initialState session
    let token : Token := { session := session, slot := 0, generation := 1 }
    let s1 := { s0 with
      registry := { s0.registry with slots := [SlotState.live 1] },
      publications := [{ slot := 0, generation := 1, state := .live }],
      snapshot := [{ slot := 0, generation := 1 }] }
    let s2 := { s1 with
      registry := { s1.registry with activeLeases := 1 },
      fastLookups := [{ id := 10, token := token }] }
    let s3 := { s2 with
      registry := { s2.registry with slots := [SlotState.vacant 2] },
      publications := [{ slot := 0, generation := 1, state := .stale }],
      snapshot := [] }
    let s4 := { s3 with
      registry := { s3.registry with activeLeases := 0 },
      fastLookups := [] }
    Step s0 .insertFresh s1 ∧
    Step s1 (.beginFastLookup 10 token) s2 ∧
    Step s2 (.removeReuse token 2) s3 ∧
    Step s3 (.completeFastLookup 10) s4 := by
  intro s0 token s1 s2 s3 s4
  have hReg1 : Registry.Step s0.registry .insertFresh s1.registry := Registry.Step.insertFresh (by rfl)
  have hStep1 : Step s0 .insertFresh s1 := Step.insertFresh hReg1 (by rfl) (by rfl)
  have hReg2 : Registry.Step s1.registry (.beginLookup token) s2.registry :=
    Registry.Step.beginLookup (by rfl) (by rfl) Nat.zero_lt_one (by rfl)
  have hStep2 : Step s1 (.beginFastLookup 10 token) s2 :=
    Step.beginFastLookup (by rfl) (by rfl) (by rfl) (by rfl) (by rfl) hReg2
  have hReg3 : Registry.Step s2.registry (.removeReuse token 2) s3.registry :=
    Registry.Step.removeReuse (by rfl) Nat.zero_lt_one (by rfl) (by rfl)
  have hStep3 : Step s2 (.removeReuse token 2) s3 :=
    Step.removeReuse hReg3 (by rfl) (by rfl)
  have hReg4 : Registry.Step s3.registry .endLookup s4.registry :=
    Registry.Step.endLookup Nat.zero_lt_one
  have hStep4 : Step s3 (.completeFastLookup 10) s4 :=
    Step.completeFastLookup (by rfl) hReg4
  exact ⟨hStep1, hStep2, hStep3, hStep4⟩

theorem slot_reuse_aba_protection_trace
    (session : SessionId) :
    let s0 := initialState session
    let token1 : Token := { session := session, slot := 0, generation := 1 }
    let token2 : Token := { session := session, slot := 0, generation := 2 }
    let s1 := { s0 with
      registry := { s0.registry with slots := [SlotState.live 1] },
      publications := [{ slot := 0, generation := 1, state := .live }],
      snapshot := [{ slot := 0, generation := 1 }] }
    let s2 := { s1 with
      registry := { s1.registry with slots := [SlotState.vacant 2] },
      publications := [{ slot := 0, generation := 1, state := .stale }],
      snapshot := [] }
    let s3 := { s2 with
      registry := { s2.registry with slots := [SlotState.live 2] },
      publications := [{ slot := 0, generation := 1, state := .stale }, { slot := 0, generation := 2, state := .live }],
      snapshot := [{ slot := 0, generation := 2 }] }
    let s4 := { s3 with
      registry := { s3.registry with activeLeases := 1 },
      fastLookups := [{ id := 20, token := token2 }] }
    Step s0 .insertFresh s1 ∧
    Step s1 (.removeReuse token1 2) s2 ∧
    Step s2 (.insertReuse 0 2) s3 ∧
    (¬ ∃ r s', Step s3 (.beginFastLookup r token1) s') ∧
    Step s3 (.beginFastLookup 20 token2) s4 := by
  intro s0 token1 token2 s1 s2 s3 s4
  have hReg1 : Registry.Step s0.registry .insertFresh s1.registry := Registry.Step.insertFresh (by rfl)
  have hStep1 : Step s0 .insertFresh s1 := Step.insertFresh hReg1 (by rfl) (by rfl)
  have hReg2 : Registry.Step s1.registry (.removeReuse token1 2) s2.registry :=
    Registry.Step.removeReuse (by rfl) Nat.zero_lt_one (by rfl) (by rfl)
  have hStep2 : Step s1 (.removeReuse token1 2) s2 :=
    Step.removeReuse hReg2 (by rfl) (by rfl)
  have hReg3 : Registry.Step s2.registry (.insertReuse 0 2) s3.registry :=
    Registry.Step.insertReuse (by rfl) Nat.zero_lt_one (by rfl)
  have hStep3 : Step s2 (.insertReuse 0 2) s3 :=
    Step.insertReuse hReg3 (by rfl) (by rfl)
  have hAba : ¬ ∃ r s', Step s3 (.beginFastLookup r token1) s' := by
    intro ⟨r, s', hStep⟩
    cases hStep with
    | beginFastLookup _ hSnap hSnapGen _ _ _ =>
        dsimp [State.findSnapshot?] at hSnap
        injection hSnap with hEq
        rw [← hEq] at hSnapGen
        dsimp at hSnapGen
        contradiction
  have hReg4 : Registry.Step s3.registry (.beginLookup token2) s4.registry :=
    Registry.Step.beginLookup (by rfl) (by rfl) Nat.zero_lt_one (by rfl)
  have hStep4 : Step s3 (.beginFastLookup 20 token2) s4 :=
    Step.beginFastLookup (by rfl) (by rfl) (by rfl) (by rfl) (by rfl) hReg4
  exact ⟨hStep1, hStep2, hStep3, hAba, hStep4⟩

theorem weak_upgrade_fallback_trace
    (session : SessionId) :
    let s0 := initialState session
    let token : Token := { session := session, slot := 0, generation := 1 }
    let s1 := { s0 with
      registry := { s0.registry with slots := [SlotState.live 1] },
      publications := [{ slot := 0, generation := 1, state := .live }],
      snapshot := [{ slot := 0, generation := 1 }] }
    let s2 := { s1 with
      registry := { s1.registry with activeLeases := 1 },
      fastLookups := [{ id := 10, token := token }] }
    let s3 := { s2 with
      registry := { s2.registry with activeLeases := 0 },
      fastLookups := [] }
    let s4 := { s3 with
      registry := { s3.registry with activeLeases := 1 } }
    let s5 := { s4 with
      registry := { s4.registry with activeLeases := 0 } }
    let s6 := { s5 with
      registry := { s5.registry with
        closed := true,
        slots := [SlotState.vacant 2] },
      publications := [{ slot := 0, generation := 1, state := .closing }],
      snapshot := [] }
    Step s0 .insertFresh s1 ∧
    Step s1 (.beginFastLookup 10 token) s2 ∧
    Step s2 (.fallbackFastLookup 10) s3 ∧
    Step s3 (.beginSlowLookup token) s4 ∧
    Step s4 .endSlowLookup s5 ∧
    Step s5 .closeRegistry s6 ∧
    Step s6 .finishClose s6 ∧
    CloseCertified s6.registry := by
  intro s0 token s1 s2 s3 s4 s5 s6
  have hReg1 : Registry.Step s0.registry .insertFresh s1.registry := Registry.Step.insertFresh (by rfl)
  have hStep1 : Step s0 .insertFresh s1 := Step.insertFresh hReg1 (by rfl) (by rfl)
  have hReg2 : Registry.Step s1.registry (.beginLookup token) s2.registry :=
    Registry.Step.beginLookup (by rfl) (by rfl) Nat.zero_lt_one (by rfl)
  have hStep2 : Step s1 (.beginFastLookup 10 token) s2 :=
    Step.beginFastLookup (by rfl) (by rfl) (by rfl) (by rfl) (by rfl) hReg2
  have hReg3 : Registry.Step s2.registry .endLookup s3.registry :=
    Registry.Step.endLookup Nat.zero_lt_one
  have hStep3 : Step s2 (.fallbackFastLookup 10) s3 :=
    Step.fallbackFastLookup (by rfl) hReg3
  have hReg4 : Registry.Step s3.registry (.beginLookup token) s4.registry :=
    Registry.Step.beginLookup (by rfl) (by rfl) Nat.zero_lt_one (by rfl)
  have hStep4 : Step s3 (.beginSlowLookup token) s4 :=
    Step.beginSlowLookup hReg4
  have hReg5 : Registry.Step s4.registry .endLookup s5.registry :=
    Registry.Step.endLookup Nat.zero_lt_one
  have hStep5 : Step s4 .endSlowLookup s5 :=
    Step.endSlowLookup Nat.zero_lt_one hReg5
  have hReg6 : Registry.Step s5.registry .closeRegistry s6.registry :=
    Registry.Step.closeRegistry (by rfl)
  have hStep6 : Step s5 .closeRegistry s6 := Step.closeRegistry hReg6
  have hReg7 : Registry.Step s6.registry .finishClose s6.registry :=
    Registry.Step.finishClose (by rfl) (by rfl)
  have hStep7 : Step s6 .finishClose s6 := Step.finishClose hReg7
  have hReach1 : Reachable s0 s1 := Reachable.tail (Reachable.refl s0) hStep1
  have hReach2 : Reachable s0 s2 := Reachable.tail hReach1 hStep2
  have hReach3 : Reachable s0 s3 := Reachable.tail hReach2 hStep3
  have hReach4 : Reachable s0 s4 := Reachable.tail hReach3 hStep4
  have hReach5 : Reachable s0 s5 := Reachable.tail hReach4 hStep5
  have hReach6 : Reachable s0 s6 := Reachable.tail hReach5 hStep6
  have hCert := close_certified_when_finished hReach6 hStep7
  exact ⟨hStep1, hStep2, hStep3, hStep4, hStep5, hStep6, hStep7, hCert.1⟩

end XlFnFormal.Handle.Registry.Snapshot
