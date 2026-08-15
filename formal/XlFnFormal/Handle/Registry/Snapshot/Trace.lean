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
      fastLookups := [{ id := 10, token := token, stage := .tentative }] }
    let s3 := { s2 with
      registry := { s2.registry with activeLeases := 1 },
      fastLookups := [{ id := 10, token := token, stage := .validated }] }
    let s4 := { s3 with
      registry := { s3.registry with activeLeases := 0 },
      fastLookups := [] }
    let s5 := { s4 with
      registry := { s4.registry with
        closed := true,
        slots := [SlotState.vacant 2] },
      publications := [{ slot := 0, generation := 1, state := .closing }],
      snapshot := [] }
    Step s0 .insertFresh s1 ∧
    Step s1 (.beginTentativeFastLookup 10 token) s2 ∧
    Step s2 (.validateFastLookup 10) s3 ∧
    Step s3 (.completeFastLookup 10) s4 ∧
    Step s4 .closeRegistry s5 ∧
    Step s5 .finishClose s5 ∧
    CloseCertified s5.registry := by
  intro s0 token s1 s2 s3 s4 s5
  have hReg1 : Registry.Step s0.registry .insertFresh s1.registry :=
    Registry.Step.insertFresh (by rfl)
  have hStep1 : Step s0 .insertFresh s1 :=
    Step.insertFresh hReg1 (by rfl) (by rfl)
  have hStep2 : Step s1 (.beginTentativeFastLookup 10 token) s2 :=
    Step.beginTentativeFastLookup (by rfl) (by rfl) (by rfl) (by rfl) (by rfl) (by rfl)
  have hReg3 : Registry.Step s2.registry (.beginLookup token) s3.registry :=
    Registry.Step.beginLookup (by rfl) (by rfl) Nat.zero_lt_one (by rfl)
  have hStep3 : Step s2 (.validateFastLookup 10) s3 :=
    Step.validateFastLookup (by rfl) (by rfl) hReg3
  have hReg4 : Registry.Step s3.registry .endLookup s4.registry :=
    Registry.Step.endLookup Nat.zero_lt_one
  have hStep4 : Step s3 (.completeFastLookup 10) s4 :=
    Step.completeFastLookup (by rfl) (by rfl) hReg4
  have hReg5 : Registry.Step s4.registry .closeRegistry s5.registry :=
    Registry.Step.closeRegistry (by rfl)
  have hStep5 : Step s4 .closeRegistry s5 := Step.closeRegistry hReg5
  have hReg6 : Registry.Step s5.registry .finishClose s5.registry :=
    Registry.Step.finishClose (by rfl) (by rfl)
  have hStep6 : Step s5 .finishClose s5 := Step.finishClose (by rfl) hReg6
  have hReach1 : Reachable s0 s1 := Reachable.tail (Reachable.refl s0) hStep1
  have hReach2 : Reachable s0 s2 := Reachable.tail hReach1 hStep2
  have hReach3 : Reachable s0 s3 := Reachable.tail hReach2 hStep3
  have hReach4 : Reachable s0 s4 := Reachable.tail hReach3 hStep4
  have hReach5 : Reachable s0 s5 := Reachable.tail hReach4 hStep5
  have hCert := close_certified_when_finished hReach5 hStep6
  exact ⟨hStep1, hStep2, hStep3, hStep4, hStep5, hStep6, hCert.1⟩

theorem fast_lookup_race_remove_linearization_trace
    (session : SessionId) :
    let s0 := initialState session
    let token : Token := { session := session, slot := 0, generation := 1 }
    let s1 := { s0 with
      registry := { s0.registry with slots := [SlotState.live 1] },
      publications := [{ slot := 0, generation := 1, state := .live }],
      snapshot := [{ slot := 0, generation := 1 }] }
    let s2 := { s1 with
      fastLookups := [{ id := 10, token := token, stage := .tentative }] }
    let s3 := { s2 with
      registry := { s2.registry with slots := [SlotState.vacant 2] },
      publications := [{ slot := 0, generation := 1, state := .stale }],
      snapshot := [] }
    let s4 := { s3 with
      fastLookups := [] }
    let s5 := { s4 with
      registry := { s4.registry with
        closed := true,
        slots := [SlotState.vacant 3] },
      publications := [{ slot := 0, generation := 1, state := .stale }],
      snapshot := [] }
    Step s0 .insertFresh s1 ∧
    Step s1 (.beginTentativeFastLookup 10 token) s2 ∧
    Step s2 (.removeReuse token 2) s3 ∧
    Step s3 (.rejectTentativeFastLookup 10) s4 ∧
    Step s4 .closeRegistry s5 ∧
    Step s5 .finishClose s5 ∧
    CloseCertified s5.registry := by
  intro s0 token s1 s2 s3 s4 s5
  have hReg1 : Registry.Step s0.registry .insertFresh s1.registry :=
    Registry.Step.insertFresh (by rfl)
  have hStep1 : Step s0 .insertFresh s1 :=
    Step.insertFresh hReg1 (by rfl) (by rfl)
  have hStep2 : Step s1 (.beginTentativeFastLookup 10 token) s2 :=
    Step.beginTentativeFastLookup (by rfl) (by rfl) (by rfl) (by rfl) (by rfl) (by rfl)
  have hReg3 : Registry.Step s2.registry (.removeReuse token 2) s3.registry :=
    Registry.Step.removeReuse (by rfl) Nat.zero_lt_one (by rfl) (by rfl)
  have hStep3 : Step s2 (.removeReuse token 2) s3 :=
    Step.removeReuse hReg3 (by rfl) (by rfl)
  have hStep4 : Step s3 (.rejectTentativeFastLookup 10) s4 :=
    Step.rejectTentativeFastLookup (by rfl) (by rfl)
  have hReg5 : Registry.Step s4.registry .closeRegistry s5.registry :=
    Registry.Step.closeRegistry (s := s4.registry) rfl
  have hStep5 : Step s4 .closeRegistry s5 := Step.closeRegistry hReg5
  have hReg6 : Registry.Step s5.registry .finishClose s5.registry :=
    Registry.Step.finishClose (by rfl) (by rfl)
  have hStep6 : Step s5 .finishClose s5 := Step.finishClose (by rfl) hReg6
  have hReach1 : Reachable s0 s1 := Reachable.tail (Reachable.refl s0) hStep1
  have hReach2 : Reachable s0 s2 := Reachable.tail hReach1 hStep2
  have hReach3 : Reachable s0 s3 := Reachable.tail hReach2 hStep3
  have hReach4 : Reachable s0 s4 := Reachable.tail hReach3 hStep4
  have hReach5 : Reachable s0 s5 := Reachable.tail hReach4 hStep5
  have hCert := close_certified_when_finished hReach5 hStep6
  exact ⟨hStep1, hStep2, hStep3, hStep4, hStep5, hStep6, hCert.1⟩

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
      publications := [{ slot := 0, generation := 1, state := .stale },
        { slot := 0, generation := 2, state := .live }],
      snapshot := [{ slot := 0, generation := 2 }] }
    let s4 := { s3 with
      fastLookups := [{ id := 20, token := token2, stage := .tentative }] }
    let s5 := { s4 with
      registry := { s4.registry with activeLeases := 1 },
      fastLookups := [{ id := 20, token := token2, stage := .validated }] }
    let s6 := { s5 with
      registry := { s5.registry with activeLeases := 0 },
      fastLookups := [] }
    Step s0 .insertFresh s1 ∧
    Step s1 (.removeReuse token1 2) s2 ∧
    Step s2 (.insertReuse 0 2) s3 ∧
    (¬ ∃ r s', Step s3 (.beginTentativeFastLookup r token1) s') ∧
    Step s3 (.beginTentativeFastLookup 20 token2) s4 ∧
    Step s4 (.validateFastLookup 20) s5 ∧
    Step s5 (.completeFastLookup 20) s6 := by
  intro s0 token1 token2 s1 s2 s3 s4 s5 s6
  have hReg1 : Registry.Step s0.registry .insertFresh s1.registry :=
    Registry.Step.insertFresh (by rfl)
  have hStep1 : Step s0 .insertFresh s1 :=
    Step.insertFresh hReg1 (by rfl) (by rfl)
  have hReg2 : Registry.Step s1.registry (.removeReuse token1 2) s2.registry :=
    Registry.Step.removeReuse (by rfl) Nat.zero_lt_one (by rfl) (by rfl)
  have hStep2 : Step s1 (.removeReuse token1 2) s2 :=
    Step.removeReuse hReg2 (by rfl) (by rfl)
  have hReg3 : Registry.Step s2.registry (.insertReuse 0 2) s3.registry :=
    Registry.Step.insertReuse (by rfl) Nat.zero_lt_one (by rfl)
  have hStep3 : Step s2 (.insertReuse 0 2) s3 :=
    Step.insertReuse hReg3 (by rfl) (by rfl)
  have hAba : ¬ ∃ r s', Step s3 (.beginTentativeFastLookup r token1) s' := by
    intro h
    rcases h with ⟨r, s', hStep⟩
    cases hStep with
    | beginTentativeFastLookup _ hSnap hSnapGen _ _ _ =>
        dsimp [State.findSnapshot?] at hSnap
        injection hSnap with hEq
        rw [← hEq] at hSnapGen
        simp [token1] at hSnapGen
  have hStep4 : Step s3 (.beginTentativeFastLookup 20 token2) s4 :=
    Step.beginTentativeFastLookup (by rfl) (by rfl) (by rfl) (by rfl) (by rfl) (by rfl)
  have hReg5 : Registry.Step s4.registry (.beginLookup token2) s5.registry :=
    Registry.Step.beginLookup (by rfl) (by rfl) Nat.zero_lt_one (by rfl)
  have hStep5 : Step s4 (.validateFastLookup 20) s5 :=
    Step.validateFastLookup (by rfl) (by rfl) hReg5
  have hReg6 : Registry.Step s5.registry .endLookup s6.registry :=
    Registry.Step.endLookup Nat.zero_lt_one
  have hStep6 : Step s5 (.completeFastLookup 20) s6 :=
    Step.completeFastLookup (by rfl) (by rfl) hReg6
  exact ⟨hStep1, hStep2, hStep3, hAba, hStep4, hStep5, hStep6⟩

theorem weak_upgrade_fallback_trace
    (session : SessionId) :
    let s0 := initialState session
    let token : Token := { session := session, slot := 0, generation := 1 }
    let s1 := { s0 with
      registry := { s0.registry with slots := [SlotState.live 1] },
      publications := [{ slot := 0, generation := 1, state := .live }],
      snapshot := [{ slot := 0, generation := 1 }] }
    let s2 := { s1 with
      fastLookups := [{ id := 10, token := token, stage := .tentative }] }
    let s3 := { s2 with
      registry := { s2.registry with activeLeases := 1 },
      fastLookups := [{ id := 10, token := token, stage := .validated }] }
    let s4 := { s3 with
      registry := { s3.registry with slots := [SlotState.vacant 2] },
      publications := [{ slot := 0, generation := 1, state := .stale }],
      snapshot := [] }
    let s5 := { s4 with
      registry := { s4.registry with activeLeases := 0 },
      fastLookups := [] }
    let s6 := { s5 with
      registry := { s5.registry with
        closed := true,
        slots := [SlotState.vacant 3] },
      publications := [{ slot := 0, generation := 1, state := .stale }],
      snapshot := [] }
    Step s0 .insertFresh s1 ∧
    Step s1 (.beginTentativeFastLookup 10 token) s2 ∧
    Step s2 (.validateFastLookup 10) s3 ∧
    Step s3 (.removeReuse token 2) s4 ∧
    Step s4 (.fallbackFastLookup 10) s5 ∧
    (¬ ∃ s', Step s5 (.beginSlowLookup token) s') ∧
    Step s5 .closeRegistry s6 ∧
    Step s6 .finishClose s6 ∧
    CloseCertified s6.registry := by
  intro s0 token s1 s2 s3 s4 s5 s6
  have hReg1 : Registry.Step s0.registry .insertFresh s1.registry :=
    Registry.Step.insertFresh (by rfl)
  have hStep1 : Step s0 .insertFresh s1 :=
    Step.insertFresh hReg1 (by rfl) (by rfl)
  have hStep2 : Step s1 (.beginTentativeFastLookup 10 token) s2 :=
    Step.beginTentativeFastLookup (by rfl) (by rfl) (by rfl) (by rfl) (by rfl) (by rfl)
  have hReg3 : Registry.Step s2.registry (.beginLookup token) s3.registry :=
    Registry.Step.beginLookup (by rfl) (by rfl) Nat.zero_lt_one (by rfl)
  have hStep3 : Step s2 (.validateFastLookup 10) s3 :=
    Step.validateFastLookup (by rfl) (by rfl) hReg3
  have hReg4 : Registry.Step s3.registry (.removeReuse token 2) s4.registry :=
    Registry.Step.removeReuse (by rfl) Nat.zero_lt_one (by rfl) (by rfl)
  have hStep4 : Step s3 (.removeReuse token 2) s4 :=
    Step.removeReuse hReg4 (by rfl) (by rfl)
  have hReg5 : Registry.Step s4.registry .endLookup s5.registry :=
    Registry.Step.endLookup Nat.zero_lt_one
  have hStep5 : Step s4 (.fallbackFastLookup 10) s5 :=
    Step.fallbackFastLookup (by rfl) (by rfl) hReg5
  have hRejected : ¬ ∃ s', Step s5 (.beginSlowLookup token) s' := by
    intro h
    rcases h with ⟨s', hStep⟩
    cases hStep with
    | beginSlowLookup hReg =>
        cases hReg with
        | beginLookup _ _ _ hLive =>
            simp [token, s5, s4, s3, s2, s1, s0] at hLive
  have hReg6 : Registry.Step s5.registry .closeRegistry s6.registry :=
    Registry.Step.closeRegistry (s := s5.registry) rfl
  have hStep6 : Step s5 .closeRegistry s6 := Step.closeRegistry hReg6
  have hReg7 : Registry.Step s6.registry .finishClose s6.registry :=
    Registry.Step.finishClose (by rfl) (by rfl)
  have hStep7 : Step s6 .finishClose s6 := Step.finishClose (by rfl) hReg7
  have hReach1 : Reachable s0 s1 := Reachable.tail (Reachable.refl s0) hStep1
  have hReach2 : Reachable s0 s2 := Reachable.tail hReach1 hStep2
  have hReach3 : Reachable s0 s3 := Reachable.tail hReach2 hStep3
  have hReach4 : Reachable s0 s4 := Reachable.tail hReach3 hStep4
  have hReach5 : Reachable s0 s5 := Reachable.tail hReach4 hStep5
  have hReach6 : Reachable s0 s6 := Reachable.tail hReach5 hStep6
  have hCert := close_certified_when_finished hReach6 hStep7
  exact ⟨hStep1, hStep2, hStep3, hStep4, hStep5, hRejected, hStep6, hStep7, hCert.1⟩

end XlFnFormal.Handle.Registry.Snapshot
