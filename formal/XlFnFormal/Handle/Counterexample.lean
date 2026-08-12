import XlFnFormal.Handle.Safety

set_option autoImplicit false

namespace XlFnFormal.Handle

def race_s0 (session : SessionId) : State := State.initialState session

def race1_s1 (session : SessionId) : State :=
  { session := session, phase := .«open», slots := [], activePrepares := 1, initializers := [], activeLeases := 0 }

def race1_s2 (session : SessionId) : State :=
  { session := session, phase := .«open», slots := [], activePrepares := 1, initializers := [{ id := 1, stage := .beforeInsert }], activeLeases := 0 }

theorem Step_race1_prep (session : SessionId) :
    Step (race_s0 session) Event.beginPrepare (race1_s1 session) := by
  exact Step.beginPrepare (Or.inl rfl)

theorem Step_race1_init (session : SessionId) :
    Step (race1_s1 session) (Event.beginInitialize 1) (race1_s2 session) := by
  have hPrep : (race1_s1 session).activePrepares > (race1_s1 session).initializers.length := Nat.zero_lt_one
  exact @Step.beginInitialize (race1_s1 session) 1 rfl hPrep rfl

def race2_s1 (session : SessionId) : State :=
  { session := session, phase := .registryClosed, slots := [], activePrepares := 0, initializers := [], activeLeases := 0 }

def race2_s2 (session : SessionId) : State :=
  { session := session, phase := .closed, slots := [], activePrepares := 0, initializers := [], activeLeases := 0 }

theorem Step_race2_step1 (session : SessionId) :
    Step (race_s0 session) Event.closeRegistry (race2_s1 session) := by
  have hPhase : (race_s0 session).phase = .«open» ∨ (race_s0 session).phase = .drainingPrepares := Or.inl rfl
  exact Step.closeRegistry hPhase rfl rfl

theorem Step_race2_step2 (session : SessionId) :
    Step (race2_s1 session) Event.finishClose (race2_s2 session) := by
  exact Step.finishClose rfl rfl

theorem close_wins_race_rejects_insert (session : SessionId) :
    ∃ (s1 s2 : State),
      Step (race_s0 session) Event.closeRegistry s1 ∧
      Step s1 Event.finishClose s2 ∧
      Reachable (race_s0 session) s2 ∧
      s2.CloseCertified ∧
      (∀ (s' : State), ¬ Step s1 (Event.beginInitialize 1) s') ∧
      (∀ (s' : State), ¬ Step s2 (Event.beginInitialize 1) s') := by
  refine ⟨race2_s1 session, race2_s2 session, ?_, ?_, ?_, ?_, ?_, ?_⟩
  · exact Step_race2_step1 session
  · exact Step_race2_step2 session
  · exact Reachable.step (Reachable.step Reachable.init
      (Step_race2_step1 session))
      (Step_race2_step2 session)
  · refine ⟨rfl, rfl, rfl, rfl, ?_⟩
    intro slot hMem
    cases hMem
  · intro s' hStep
    cases hStep
    rename_i hPhase _ _
    contradiction
  · intro s' hStep
    cases hStep
    rename_i hPhase _ _
    contradiction

-- Explicit H2 Draining Rollback Trace
def drain_s0 (session : SessionId) : State := State.initialState session

def drain_s1 (session : SessionId) : State :=
  { session := session, phase := .«open», slots := [], activePrepares := 1, initializers := [], activeLeases := 0 }

def drain_s2 (session : SessionId) : State :=
  { session := session, phase := .«open», slots := [], activePrepares := 1, initializers := [{ id := 1, stage := .beforeInsert }], activeLeases := 0 }

def drain_s3 (session : SessionId) : State :=
  { session := session, phase := .drainingPrepares, slots := [], activePrepares := 1, initializers := [{ id := 1, stage := .beforeInsert }], activeLeases := 0 }

def drain_s4 (session : SessionId) : State :=
  { session := session, phase := .drainingPrepares, slots := [.live 1], activePrepares := 1, initializers := [{ id := 1, stage := .pending { session := session, slot := 0, generation := 1 } }], activeLeases := 0 }

def drain_s5 (session : SessionId) : State :=
  { session := session, phase := .drainingPrepares, slots := [.vacant 2], activePrepares := 1, initializers := [{ id := 1, stage := .resolved }], activeLeases := 0 }

def drain_s6 (session : SessionId) : State :=
  { session := session, phase := .drainingPrepares, slots := [.vacant 2], activePrepares := 1, initializers := [], activeLeases := 0 }

def drain_s7 (session : SessionId) : State :=
  { session := session, phase := .drainingPrepares, slots := [.vacant 2], activePrepares := 0, initializers := [], activeLeases := 0 }

def drain_s8 (session : SessionId) : State :=
  { (drain_s7 session) with phase := .registryClosed, slots := (drain_s7 session).slots.map closeSlot }

def drain_s9 (session : SessionId) : State :=
  { (drain_s8 session) with phase := .closed }

theorem draining_race_rollback_completes_quiescent (session : SessionId) :
    ∃ s1 s2 s3 s4 s5 s6 s7 s8 s9,
      Step (drain_s0 session) Event.beginPrepare s1 ∧
      Step s1 (Event.beginInitialize 1) s2 ∧
      Step s2 Event.sealTopics s3 ∧
      Step s3 (Event.insertPendingFresh 1) s4 ∧
      Step s4 (Event.rollbackPendingReuse 1 2) s5 ∧
      Step s5 (Event.finishInitialize 1) s6 ∧
      Step s6 Event.endPrepare s7 ∧
      Step s7 Event.closeRegistry s8 ∧
      Step s8 Event.finishClose s9 ∧
      Reachable (drain_s0 session) s9 ∧
      s9.CloseCertified ∧
      (∀ s', ¬ Step s4 (Event.publishTopic 1) s') := by
  refine ⟨drain_s1 session, drain_s2 session, drain_s3 session, drain_s4 session, drain_s5 session, drain_s6 session, drain_s7 session, drain_s8 session, drain_s9 session, ?_⟩
  have step1 : Step (drain_s0 session) Event.beginPrepare (drain_s1 session) := Step.beginPrepare (Or.inl rfl)
  have hPrep1 : (drain_s1 session).activePrepares > (drain_s1 session).initializers.length := Nat.zero_lt_one
  have step2 : Step (drain_s1 session) (Event.beginInitialize 1) (drain_s2 session) := @Step.beginInitialize (drain_s1 session) 1 rfl hPrep1 rfl
  have step3 : Step (drain_s2 session) Event.sealTopics (drain_s3 session) := Step.sealTopics rfl
  have step4 : Step (drain_s3 session) (Event.insertPendingFresh 1) (drain_s4 session) := Step.insertPendingFresh rfl
  have step5 : Step (drain_s4 session) (Event.rollbackPendingReuse 1 2) (drain_s5 session) := by
    have hInBounds : 0 < (drain_s4 session).slots.length := by dsimp [drain_s4]; decide
    have hLive : (drain_s4 session).slots.get ⟨0, hInBounds⟩ = .live 1 := rfl
    have hNext : nextGeneration? 1 = some 2 := rfl
    exact @Step.rollbackPendingReuse (drain_s4 session) 1 { session := session, slot := 0, generation := 1 } 2 rfl hInBounds hLive hNext
  have step6 : Step (drain_s5 session) (Event.finishInitialize 1) (drain_s6 session) := Step.finishInitialize rfl (Or.inr rfl)
  have hPrep6 : (drain_s6 session).activePrepares > (drain_s6 session).initializers.length := Nat.zero_lt_one
  have step7 : Step (drain_s6 session) Event.endPrepare (drain_s7 session) := Step.endPrepare hPrep6
  have hPhase7 : (drain_s7 session).phase = .«open» ∨ (drain_s7 session).phase = .drainingPrepares := Or.inr rfl
  have step8 : Step (drain_s7 session) Event.closeRegistry (drain_s8 session) := Step.closeRegistry hPhase7 rfl rfl
  have step9 : Step (drain_s8 session) Event.finishClose (drain_s9 session) := Step.finishClose rfl rfl
  refine ⟨step1, step2, step3, step4, step5, step6, step7, step8, step9, ?_, ?_, ?_⟩
  · exact Reachable.step (Reachable.step (Reachable.step (Reachable.step (Reachable.step (Reachable.step (Reachable.step (Reachable.step (Reachable.step Reachable.init
      step1) step2) step3) step4) step5) step6) step7) step8) step9
  · refine ⟨rfl, rfl, rfl, rfl, ?_⟩
    intro slot hMem
    rcases List.mem_singleton.mp hMem with rfl
    exact trivial
  · intro s' hStep
    cases hStep
    rename_i hPhase _
    contradiction

end XlFnFormal.Handle
