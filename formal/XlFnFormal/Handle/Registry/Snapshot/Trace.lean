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
      fastLookups := [{ id := 10, token := token, stage := .observed }] }
    let s3 := { s2 with
      fastLookups := [{ id := 10, token := token, stage := .tentative }] }
    let s4 := { s3 with
      registry := { s3.registry with activeLeases := 1 },
      fastLookups := [{ id := 10, token := token, stage := .validated }] }
    let s5 := { s4 with
      registry := { s4.registry with activeLeases := 0 },
      fastLookups := [] }
    let s6 := { s5 with
      leaseAdmission := .sealing }
    let s7 := { s6 with
      leaseAdmission := .sealed }
    let s8 := { s7 with
      registry := { s7.registry with
        closed := true,
        slots := [SlotState.vacant 2] },
      publications := [{ slot := 0, generation := 1, state := .closing }],
      snapshot := [] }
    Step s0 .insertFresh s1 ∧
    Step s1 (.beginFastObservation 10 token) s2 ∧
    Step s2 (.acquireTentativeLease 10) s3 ∧
    Step s3 (.validateFastLookup 10) s4 ∧
    Step s4 (.completeFastLookup 10) s5 ∧
    Step s5 .beginSealLeaseAdmission s6 ∧
    Step s6 .finishSealLeaseAdmission s7 ∧
    Step s7 .closeRegistry s8 ∧
    Step s8 .finishClose s8 ∧
    CloseCertified s8.registry := by
  intro s0 token s1 s2 s3 s4 s5 s6 s7 s8
  have hReg1 : Registry.Step s0.registry .insertFresh s1.registry :=
    Registry.Step.insertFresh (by rfl)
  have hStep1 : Step s0 .insertFresh s1 :=
    Step.insertFresh hReg1 (by rfl) (by rfl)
  have hStep2 : Step s1 (.beginFastObservation 10 token) s2 :=
    Step.beginFastObservation (by rfl) (by rfl) (by rfl) (by rfl) (by rfl) (by rfl)
  have hStep3 : Step s2 (.acquireTentativeLease 10) s3 :=
    Step.acquireTentativeLease (by rfl) (by rfl) (by intro h; cases h) (by rfl)
  have hReg4 : Registry.Step s3.registry (.beginLookup token) s4.registry :=
    Registry.Step.beginLookup (by rfl) (by rfl) Nat.zero_lt_one (by rfl)
  have hStep4 : Step s3 (.validateFastLookup 10) s4 :=
    Step.validateFastLookup (by rfl) (by rfl) (by rfl) (by rfl) hReg4
  have hReg5 : Registry.Step s4.registry .endLookup s5.registry :=
    Registry.Step.endLookup Nat.zero_lt_one
  have hStep5 : Step s4 (.completeFastLookup 10) s5 :=
    Step.completeFastLookup (by rfl) (by rfl) hReg5
  have hStep6 : Step s5 .beginSealLeaseAdmission s6 :=
    Step.beginSealLeaseAdmission (by rfl)
  have hStep7 : Step s6 .finishSealLeaseAdmission s7 :=
    Step.finishSealLeaseAdmission (by rfl)
  have hReg8 : Registry.Step s7.registry .closeRegistry s8.registry :=
    Registry.Step.closeRegistry (by rfl)
  have hStep8 : Step s7 .closeRegistry s8 := Step.closeRegistry (by rfl) hReg8
  have hReg9 : Registry.Step s8.registry .finishClose s8.registry :=
    Registry.Step.finishClose (by rfl) (by rfl)
  have hStep9 : Step s8 .finishClose s8 := Step.finishClose (by rfl) (by rfl) hReg9
  have hReach1 : Reachable s0 s1 := Reachable.tail (Reachable.refl s0) hStep1
  have hReach2 : Reachable s0 s2 := Reachable.tail hReach1 hStep2
  have hReach3 : Reachable s0 s3 := Reachable.tail hReach2 hStep3
  have hReach4 : Reachable s0 s4 := Reachable.tail hReach3 hStep4
  have hReach5 : Reachable s0 s5 := Reachable.tail hReach4 hStep5
  have hReach6 : Reachable s0 s6 := Reachable.tail hReach5 hStep6
  have hReach7 : Reachable s0 s7 := Reachable.tail hReach6 hStep7
  have hReach8 : Reachable s0 s8 := Reachable.tail hReach7 hStep8
  have hCert := close_certified_when_finished hReach8 hStep9
  exact ⟨hStep1, hStep2, hStep3, hStep4, hStep5, hStep6, hStep7, hStep8, hStep9, hCert.1⟩

theorem non_blocking_lease_admission_race_trace
    (session : SessionId) :
    let s0 := initialState session
    let token : Token := { session := session, slot := 0, generation := 1 }
    let s1 := { s0 with
      registry := { s0.registry with slots := [SlotState.live 1] },
      publications := [{ slot := 0, generation := 1, state := .live }],
      snapshot := [{ slot := 0, generation := 1 }] }
    let s2 := { s1 with
      fastLookups := [{ id := 10, token := token, stage := .observed }] }
    let s3 := { s2 with
      fastLookups := [{ id := 10, token := token, stage := .tentative }] }
    let s4 := { s3 with
      fastLookups := [{ id := 10, token := token, stage := .tentative },
                      { id := 20, token := token, stage := .observed }] }
    let s5 := { s4 with
      leaseAdmission := .sealing }
    let s6 := { s5 with
      leaseAdmission := .sealed }
    let s7 := { s6 with
      fastLookups := [{ id := 10, token := token, stage := .tentative }] }
    let s8 := { s7 with
      registry := { s7.registry with activeLeases := 1 },
      fastLookups := [{ id := 10, token := token, stage := .validated }] }
    let s9 := { s8 with
      registry := { s8.registry with activeLeases := 0 },
      fastLookups := [] }
    let s10 := { s9 with
      registry := { s9.registry with
        closed := true,
        slots := [SlotState.vacant 2] },
      publications := [{ slot := 0, generation := 1, state := .closing }],
      snapshot := [] }
    Step s0 .insertFresh s1 ∧
    Step s1 (.beginFastObservation 10 token) s2 ∧
    Step s2 (.acquireTentativeLease 10) s3 ∧
    Step s3 (.beginFastObservation 20 token) s4 ∧
    Step s4 .beginSealLeaseAdmission s5 ∧
    Step s5 .finishSealLeaseAdmission s6 ∧
    (¬ ∃ s', Step s6 (.acquireTentativeLease 20) s') ∧
    Step s6 (.abandonObservation 20) s7 ∧
    Step s7 (.validateFastLookup 10) s8 ∧
    Step s8 (.completeFastLookup 10) s9 ∧
    Step s9 .closeRegistry s10 ∧
    Step s10 .finishClose s10 ∧
    CloseCertified s10.registry := by
  intro s0 token s1 s2 s3 s4 s5 s6 s7 s8 s9 s10
  have hReg1 : Registry.Step s0.registry .insertFresh s1.registry :=
    Registry.Step.insertFresh (by rfl)
  have hStep1 : Step s0 .insertFresh s1 :=
    Step.insertFresh hReg1 (by rfl) (by rfl)
  have hStep2 : Step s1 (.beginFastObservation 10 token) s2 :=
    Step.beginFastObservation (by rfl) (by rfl) (by rfl) (by rfl) (by rfl) (by rfl)
  have hStep3 : Step s2 (.acquireTentativeLease 10) s3 :=
    Step.acquireTentativeLease (by rfl) (by rfl) (by intro h; cases h) (by rfl)
  have hStep4 : Step s3 (.beginFastObservation 20 token) s4 :=
    Step.beginFastObservation (by rfl) (by rfl) (by rfl) (by rfl) (by rfl) (by rfl)
  have hStep5 : Step s4 .beginSealLeaseAdmission s5 :=
    Step.beginSealLeaseAdmission (by rfl)
  have hStep6 : Step s5 .finishSealLeaseAdmission s6 :=
    Step.finishSealLeaseAdmission (by rfl)
  have hAcq20Rejected : ¬ ∃ s', Step s6 (.acquireTentativeLease 20) s' :=
    sealed_admission_rejects_tentative_lease_acquisition rfl 20
  have hStep7 : Step s6 (.abandonObservation 20) s7 :=
    Step.abandonObservation (by rfl) (by rfl)
  have hReg8 : Registry.Step s7.registry (.beginLookup token) s8.registry :=
    Registry.Step.beginLookup (by rfl) (by rfl) Nat.zero_lt_one (by rfl)
  have hStep8 : Step s7 (.validateFastLookup 10) s8 :=
    Step.validateFastLookup (by rfl) (by rfl) (by rfl) (by rfl) hReg8
  have hReg9 : Registry.Step s8.registry .endLookup s9.registry :=
    Registry.Step.endLookup Nat.zero_lt_one
  have hStep9 : Step s8 (.completeFastLookup 10) s9 :=
    Step.completeFastLookup (by rfl) (by rfl) hReg9
  have hReg10 : Registry.Step s9.registry .closeRegistry s10.registry :=
    Registry.Step.closeRegistry (by rfl)
  have hStep10 : Step s9 .closeRegistry s10 := Step.closeRegistry (by rfl) hReg10
  have hReg11 : Registry.Step s10.registry .finishClose s10.registry :=
    Registry.Step.finishClose (by rfl) (by rfl)
  have hStep11 : Step s10 .finishClose s10 := Step.finishClose (by rfl) (by rfl) hReg11
  have hReach1 : Reachable s0 s1 := Reachable.tail (Reachable.refl s0) hStep1
  have hReach2 : Reachable s0 s2 := Reachable.tail hReach1 hStep2
  have hReach3 : Reachable s0 s3 := Reachable.tail hReach2 hStep3
  have hReach4 : Reachable s0 s4 := Reachable.tail hReach3 hStep4
  have hReach5 : Reachable s0 s5 := Reachable.tail hReach4 hStep5
  have hReach6 : Reachable s0 s6 := Reachable.tail hReach5 hStep6
  have hReach7 : Reachable s0 s7 := Reachable.tail hReach6 hStep7
  have hReach8 : Reachable s0 s8 := Reachable.tail hReach7 hStep8
  have hReach9 : Reachable s0 s9 := Reachable.tail hReach8 hStep9
  have hReach10 : Reachable s0 s10 := Reachable.tail hReach9 hStep10
  have hCert := close_certified_when_finished hReach10 hStep11
  exact ⟨hStep1, hStep2, hStep3, hStep4, hStep5, hStep6, hAcq20Rejected, hStep7, hStep8, hStep9, hStep10, hStep11, hCert.1⟩

theorem fast_lookup_close_before_lease_acquire_trace
    (session : SessionId) :
    let s0 := initialState session
    let token : Token := { session := session, slot := 0, generation := 1 }
    let s1 := { s0 with
      registry := { s0.registry with slots := [SlotState.live 1] },
      publications := [{ slot := 0, generation := 1, state := .live }],
      snapshot := [{ slot := 0, generation := 1 }] }
    let s2 := { s1 with
      fastLookups := [{ id := 10, token := token, stage := .observed }] }
    let s3 := { s2 with
      leaseAdmission := .sealing }
    let s4 := { s3 with
      leaseAdmission := .sealed }
    let s5 := { s4 with
      fastLookups := [] }
    let s6 := { s5 with
      registry := { s5.registry with
        closed := true,
        slots := [SlotState.vacant 2] },
      publications := [{ slot := 0, generation := 1, state := .closing }],
      snapshot := [] }
    Step s0 .insertFresh s1 ∧
    Step s1 (.beginFastObservation 10 token) s2 ∧
    Step s2 .beginSealLeaseAdmission s3 ∧
    Step s3 .finishSealLeaseAdmission s4 ∧
    (¬ ∃ s', Step s4 (.acquireTentativeLease 10) s') ∧
    Step s4 (.abandonObservation 10) s5 ∧
    Step s5 .closeRegistry s6 ∧
    Step s6 .finishClose s6 ∧
    CloseCertified s6.registry := by
  intro s0 token s1 s2 s3 s4 s5 s6
  have hReg1 : Registry.Step s0.registry .insertFresh s1.registry :=
    Registry.Step.insertFresh (by rfl)
  have hStep1 : Step s0 .insertFresh s1 :=
    Step.insertFresh hReg1 (by rfl) (by rfl)
  have hStep2 : Step s1 (.beginFastObservation 10 token) s2 :=
    Step.beginFastObservation (by rfl) (by rfl) (by rfl) (by rfl) (by rfl) (by rfl)
  have hStep3 : Step s2 .beginSealLeaseAdmission s3 :=
    Step.beginSealLeaseAdmission (by rfl)
  have hStep4 : Step s3 .finishSealLeaseAdmission s4 :=
    Step.finishSealLeaseAdmission (by rfl)
  have hAcqRejected : ¬ ∃ s', Step s4 (.acquireTentativeLease 10) s' :=
    sealed_admission_rejects_tentative_lease_acquisition rfl 10
  have hStep5 : Step s4 (.abandonObservation 10) s5 :=
    Step.abandonObservation (by rfl) (by rfl)
  have hReg6 : Registry.Step s5.registry .closeRegistry s6.registry :=
    Registry.Step.closeRegistry (s := s5.registry) rfl
  have hStep6 : Step s5 .closeRegistry s6 := Step.closeRegistry (by rfl) hReg6
  have hReg7 : Registry.Step s6.registry .finishClose s6.registry :=
    Registry.Step.finishClose (by rfl) (by rfl)
  have hStep7 : Step s6 .finishClose s6 := Step.finishClose (by rfl) (by rfl) hReg7
  have hReach1 : Reachable s0 s1 := Reachable.tail (Reachable.refl s0) hStep1
  have hReach2 : Reachable s0 s2 := Reachable.tail hReach1 hStep2
  have hReach3 : Reachable s0 s3 := Reachable.tail hReach2 hStep3
  have hReach4 : Reachable s0 s4 := Reachable.tail hReach3 hStep4
  have hReach5 : Reachable s0 s5 := Reachable.tail hReach4 hStep5
  have hReach6 : Reachable s0 s6 := Reachable.tail hReach5 hStep6
  have hCert := close_certified_when_finished hReach6 hStep7
  exact ⟨hStep1, hStep2, hStep3, hStep4, hAcqRejected, hStep5, hStep6, hStep7, hCert.1⟩

theorem fast_lookup_race_remove_linearization_trace
    (session : SessionId) :
    let s0 := initialState session
    let token : Token := { session := session, slot := 0, generation := 1 }
    let s1 := { s0 with
      registry := { s0.registry with slots := [SlotState.live 1] },
      publications := [{ slot := 0, generation := 1, state := .live }],
      snapshot := [{ slot := 0, generation := 1 }] }
    let s2 := { s1 with
      fastLookups := [{ id := 10, token := token, stage := .observed }] }
    let s3 := { s2 with
      fastLookups := [{ id := 10, token := token, stage := .tentative }] }
    let s4 := { s3 with
      registry := { s3.registry with slots := [SlotState.vacant 2] },
      publications := [{ slot := 0, generation := 1, state := .stale }],
      snapshot := [] }
    let s5 := { s4 with
      fastLookups := [] }
    let s6 := { s5 with
      leaseAdmission := .sealing }
    let s7 := { s6 with
      leaseAdmission := .sealed }
    let s8 := { s7 with
      registry := { s7.registry with
        closed := true,
        slots := [SlotState.vacant 3] },
      publications := [{ slot := 0, generation := 1, state := .stale }],
      snapshot := [] }
    Step s0 .insertFresh s1 ∧
    Step s1 (.beginFastObservation 10 token) s2 ∧
    Step s2 (.acquireTentativeLease 10) s3 ∧
    Step s3 (.removeReuse token 2) s4 ∧
    Step s4 (.rejectTentativeFastLookup 10) s5 ∧
    Step s5 .beginSealLeaseAdmission s6 ∧
    Step s6 .finishSealLeaseAdmission s7 ∧
    Step s7 .closeRegistry s8 ∧
    Step s8 .finishClose s8 ∧
    CloseCertified s8.registry := by
  intro s0 token s1 s2 s3 s4 s5 s6 s7 s8
  have hReg1 : Registry.Step s0.registry .insertFresh s1.registry :=
    Registry.Step.insertFresh (by rfl)
  have hStep1 : Step s0 .insertFresh s1 :=
    Step.insertFresh hReg1 (by rfl) (by rfl)
  have hStep2 : Step s1 (.beginFastObservation 10 token) s2 :=
    Step.beginFastObservation (by rfl) (by rfl) (by rfl) (by rfl) (by rfl) (by rfl)
  have hStep3 : Step s2 (.acquireTentativeLease 10) s3 :=
    Step.acquireTentativeLease (by rfl) (by rfl) (by intro h; cases h) (by rfl)
  have hReg4 : Registry.Step s3.registry (.removeReuse token 2) s4.registry :=
    Registry.Step.removeReuse (by rfl) Nat.zero_lt_one (by rfl) (by rfl)
  have hStep4 : Step s3 (.removeReuse token 2) s4 :=
    Step.removeReuse hReg4 (by rfl) (by rfl)
  have hStep5 : Step s4 (.rejectTentativeFastLookup 10) s5 :=
    Step.rejectTentativeFastLookup (by rfl) (by rfl) (by rfl) (by simp)
  have hStep6 : Step s5 .beginSealLeaseAdmission s6 :=
    Step.beginSealLeaseAdmission (by rfl)
  have hStep7 : Step s6 .finishSealLeaseAdmission s7 :=
    Step.finishSealLeaseAdmission (by rfl)
  have hReg8 : Registry.Step s7.registry .closeRegistry s8.registry :=
    Registry.Step.closeRegistry (s := s7.registry) rfl
  have hStep8 : Step s7 .closeRegistry s8 := Step.closeRegistry (by rfl) hReg8
  have hReg9 : Registry.Step s8.registry .finishClose s8.registry :=
    Registry.Step.finishClose (by rfl) (by rfl)
  have hStep9 : Step s8 .finishClose s8 := Step.finishClose (by rfl) (by rfl) hReg9
  have hReach1 : Reachable s0 s1 := Reachable.tail (Reachable.refl s0) hStep1
  have hReach2 : Reachable s0 s2 := Reachable.tail hReach1 hStep2
  have hReach3 : Reachable s0 s3 := Reachable.tail hReach2 hStep3
  have hReach4 : Reachable s0 s4 := Reachable.tail hReach3 hStep4
  have hReach5 : Reachable s0 s5 := Reachable.tail hReach4 hStep5
  have hReach6 : Reachable s0 s6 := Reachable.tail hReach5 hStep6
  have hReach7 : Reachable s0 s7 := Reachable.tail hReach6 hStep7
  have hReach8 : Reachable s0 s8 := Reachable.tail hReach7 hStep8
  have hCert := close_certified_when_finished hReach8 hStep9
  exact ⟨hStep1, hStep2, hStep3, hStep4, hStep5, hStep6, hStep7, hStep8, hStep9, hCert.1⟩

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
      fastLookups := [{ id := 20, token := token2, stage := .observed }] }
    let s5 := { s4 with
      fastLookups := [{ id := 20, token := token2, stage := .tentative }] }
    let s6 := { s5 with
      registry := { s5.registry with activeLeases := 1 },
      fastLookups := [{ id := 20, token := token2, stage := .validated }] }
    let s7 := { s6 with
      registry := { s6.registry with activeLeases := 0 },
      fastLookups := [] }
    Step s0 .insertFresh s1 ∧
    Step s1 (.removeReuse token1 2) s2 ∧
    Step s2 (.insertReuse 0 2) s3 ∧
    (¬ ∃ r s', Step s3 (.beginFastObservation r token1) s') ∧
    Step s3 (.beginFastObservation 20 token2) s4 ∧
    Step s4 (.acquireTentativeLease 20) s5 ∧
    Step s5 (.validateFastLookup 20) s6 ∧
    Step s6 (.completeFastLookup 20) s7 := by
  intro s0 token1 token2 s1 s2 s3 s4 s5 s6 s7
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
  have hAba : ¬ ∃ r s', Step s3 (.beginFastObservation r token1) s' := by
    intro h
    rcases h with ⟨r, s', hStep⟩
    cases hStep with
    | beginFastObservation _ hSnap hSnapGen _ _ _ =>
        dsimp [State.findSnapshot?] at hSnap
        injection hSnap with hEq
        rw [← hEq] at hSnapGen
        simp [token1] at hSnapGen
  have hStep4 : Step s3 (.beginFastObservation 20 token2) s4 :=
    Step.beginFastObservation (by rfl) (by rfl) (by rfl) (by rfl) (by rfl) (by rfl)
  have hStep5 : Step s4 (.acquireTentativeLease 20) s5 :=
    Step.acquireTentativeLease (by rfl) (by rfl) (by intro h; cases h) (by rfl)
  have hReg6 : Registry.Step s5.registry (.beginLookup token2) s6.registry :=
    Registry.Step.beginLookup (by rfl) (by rfl) Nat.zero_lt_one (by rfl)
  have hStep6 : Step s5 (.validateFastLookup 20) s6 :=
    Step.validateFastLookup (by rfl) (by rfl) (by rfl) (by rfl) hReg6
  have hReg7 : Registry.Step s6.registry .endLookup s7.registry :=
    Registry.Step.endLookup Nat.zero_lt_one
  have hStep7 : Step s6 (.completeFastLookup 20) s7 :=
    Step.completeFastLookup (by rfl) (by rfl) hReg7
  exact ⟨hStep1, hStep2, hStep3, hAba, hStep4, hStep5, hStep6, hStep7⟩

theorem weak_upgrade_fallback_trace
    (session : SessionId) :
    let s0 := initialState session
    let token : Token := { session := session, slot := 0, generation := 1 }
    let s1 := { s0 with
      registry := { s0.registry with slots := [SlotState.live 1] },
      publications := [{ slot := 0, generation := 1, state := .live }],
      snapshot := [{ slot := 0, generation := 1 }] }
    let s2 := { s1 with
      fastLookups := [{ id := 10, token := token, stage := .observed }] }
    let s3 := { s2 with
      fastLookups := [{ id := 10, token := token, stage := .tentative }] }
    let s4 := { s3 with
      registry := { s3.registry with activeLeases := 1 },
      fastLookups := [{ id := 10, token := token, stage := .validated }] }
    let s5 := { s4 with
      registry := { s4.registry with slots := [SlotState.vacant 2] },
      publications := [{ slot := 0, generation := 1, state := .stale }],
      snapshot := [] }
    let s6 := { s5 with
      registry := { s5.registry with activeLeases := 0 },
      fastLookups := [] }
    let s7 := { s6 with
      leaseAdmission := .sealing }
    let s8 := { s7 with
      leaseAdmission := .sealed }
    let s9 := { s8 with
      registry := { s8.registry with
        closed := true,
        slots := [SlotState.vacant 3] },
      publications := [{ slot := 0, generation := 1, state := .stale }],
      snapshot := [] }
    Step s0 .insertFresh s1 ∧
    Step s1 (.beginFastObservation 10 token) s2 ∧
    Step s2 (.acquireTentativeLease 10) s3 ∧
    Step s3 (.validateFastLookup 10) s4 ∧
    Step s4 (.removeReuse token 2) s5 ∧
    Step s5 (.fallbackFastLookup 10) s6 ∧
    (¬ ∃ s', Step s6 (.beginSlowLookup token) s') ∧
    Step s6 .beginSealLeaseAdmission s7 ∧
    Step s7 .finishSealLeaseAdmission s8 ∧
    Step s8 .closeRegistry s9 ∧
    Step s9 .finishClose s9 ∧
    CloseCertified s9.registry := by
  intro s0 token s1 s2 s3 s4 s5 s6 s7 s8 s9
  have hReg1 : Registry.Step s0.registry .insertFresh s1.registry :=
    Registry.Step.insertFresh (by rfl)
  have hStep1 : Step s0 .insertFresh s1 :=
    Step.insertFresh hReg1 (by rfl) (by rfl)
  have hStep2 : Step s1 (.beginFastObservation 10 token) s2 :=
    Step.beginFastObservation (by rfl) (by rfl) (by rfl) (by rfl) (by rfl) (by rfl)
  have hStep3 : Step s2 (.acquireTentativeLease 10) s3 :=
    Step.acquireTentativeLease (by rfl) (by rfl) (by intro h; cases h) (by rfl)
  have hReg4 : Registry.Step s3.registry (.beginLookup token) s4.registry :=
    Registry.Step.beginLookup (by rfl) (by rfl) Nat.zero_lt_one (by rfl)
  have hStep4 : Step s3 (.validateFastLookup 10) s4 :=
    Step.validateFastLookup (by rfl) (by rfl) (by rfl) (by rfl) hReg4
  have hReg5 : Registry.Step s4.registry (.removeReuse token 2) s5.registry :=
    Registry.Step.removeReuse (by rfl) Nat.zero_lt_one (by rfl) (by rfl)
  have hStep5 : Step s4 (.removeReuse token 2) s5 :=
    Step.removeReuse hReg5 (by rfl) (by rfl)
  have hReg6 : Registry.Step s5.registry .endLookup s6.registry :=
    Registry.Step.endLookup Nat.zero_lt_one
  have hStep6 : Step s5 (.fallbackFastLookup 10) s6 :=
    Step.fallbackFastLookup (by rfl) (by rfl) (by rfl) (by simp) hReg6
  have hRejected : ¬ ∃ s', Step s6 (.beginSlowLookup token) s' := by
    intro h
    rcases h with ⟨s', hStep⟩
    cases hStep with
    | beginSlowLookup _ hReg =>
        cases hReg with
        | beginLookup _ _ _ hLive =>
            simp [token, s6, s5, s4, s3, s2, s1, s0] at hLive
  have hStep7 : Step s6 .beginSealLeaseAdmission s7 :=
    Step.beginSealLeaseAdmission (by rfl)
  have hStep8 : Step s7 .finishSealLeaseAdmission s8 :=
    Step.finishSealLeaseAdmission (by rfl)
  have hReg9 : Registry.Step s8.registry .closeRegistry s9.registry :=
    Registry.Step.closeRegistry (s := s8.registry) rfl
  have hStep9 : Step s8 .closeRegistry s9 := Step.closeRegistry (by rfl) hReg9
  have hReg10 : Registry.Step s9.registry .finishClose s9.registry :=
    Registry.Step.finishClose (by rfl) (by rfl)
  have hStep10 : Step s9 .finishClose s9 := Step.finishClose (by rfl) (by rfl) hReg10
  have hReach1 : Reachable s0 s1 := Reachable.tail (Reachable.refl s0) hStep1
  have hReach2 : Reachable s0 s2 := Reachable.tail hReach1 hStep2
  have hReach3 : Reachable s0 s3 := Reachable.tail hReach2 hStep3
  have hReach4 : Reachable s0 s4 := Reachable.tail hReach3 hStep4
  have hReach5 : Reachable s0 s5 := Reachable.tail hReach4 hStep5
  have hReach6 : Reachable s0 s6 := Reachable.tail hReach5 hStep6
  have hReach7 : Reachable s0 s7 := Reachable.tail hReach6 hStep7
  have hReach8 : Reachable s0 s8 := Reachable.tail hReach7 hStep8
  have hReach9 : Reachable s0 s9 := Reachable.tail hReach8 hStep9
  have hCert := close_certified_when_finished hReach9 hStep10
  exact ⟨hStep1, hStep2, hStep3, hStep4, hStep5, hStep6, hRejected, hStep7, hStep8, hStep9, hStep10, hCert.1⟩

end XlFnFormal.Handle.Registry.Snapshot
