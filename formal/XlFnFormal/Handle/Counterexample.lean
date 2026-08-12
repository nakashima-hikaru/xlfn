import XlFnFormal.Handle.Safety

set_option autoImplicit false

namespace XlFnFormal.Handle

def race_s0 (session : SessionId) : State := State.initialState session

def race1_s1 (session : SessionId) : State :=
  { session := session, phase := .«open», slots := [.live 1], activePrepares := 0, activeInitializers := 0, activeLeases := 0 }

def race1_s2 (session : SessionId) : State :=
  { session := session, phase := .registryClosed, slots := [.vacant 2], activePrepares := 0, activeInitializers := 0, activeLeases := 0 }

def race1_s3 (session : SessionId) : State :=
  { session := session, phase := .closed, slots := [.vacant 2], activePrepares := 0, activeInitializers := 0, activeLeases := 0 }

theorem Step_race1_step1 (session : SessionId) :
    Step (race_s0 session) Event.insertFresh (race1_s1 session) := by
  have hMay : (race_s0 session).MayInsert := Or.inl rfl
  exact Step.insertFresh hMay

theorem Step_race1_step2 (session : SessionId) :
    Step (race1_s1 session) Event.closeRegistry (race1_s2 session) := by
  exact Step.closeRegistry (Or.inl rfl) rfl rfl

theorem Step_race1_step3 (session : SessionId) :
    Step (race1_s2 session) Event.finishClose (race1_s3 session) := by
  exact Step.finishClose rfl rfl

theorem insert_wins_race_is_quiescent (session : SessionId) :
    ∃ (s1 s2 s3 : State),
      Step (race_s0 session) Event.insertFresh s1 ∧
      Step s1 Event.closeRegistry s2 ∧
      Step s2 Event.finishClose s3 ∧
      Reachable (race_s0 session) s3 ∧
      s3.CloseCertified := by
  refine ⟨race1_s1 session, race1_s2 session, race1_s3 session, ?_, ?_, ?_, ?_, ?_⟩
  · exact Step_race1_step1 session
  · exact Step_race1_step2 session
  · exact Step_race1_step3 session
  · exact Reachable.step (Reachable.step (Reachable.step Reachable.init
      (Step_race1_step1 session))
      (Step_race1_step2 session))
      (Step_race1_step3 session)
  · refine ⟨rfl, rfl, rfl, rfl, ?_⟩
    intro slot hMem
    rcases List.mem_singleton.mp hMem with rfl
    exact trivial

def race2_s1 (session : SessionId) : State :=
  { session := session, phase := .registryClosed, slots := [], activePrepares := 0, activeInitializers := 0, activeLeases := 0 }

def race2_s2 (session : SessionId) : State :=
  { session := session, phase := .closed, slots := [], activePrepares := 0, activeInitializers := 0, activeLeases := 0 }

theorem Step_race2_step1 (session : SessionId) :
    Step (race_s0 session) Event.closeRegistry (race2_s1 session) := by
  have hPhase : (race_s0 session).phase = .«open» ∨ (race_s0 session).phase = .drainingPrepares := Or.inl rfl
  have hNoInits : (race_s0 session).activeInitializers = 0 := rfl
  have hNoPrepares : (race_s0 session).activePrepares = 0 := rfl
  exact Step.closeRegistry hPhase hNoInits hNoPrepares

theorem Step_race2_step2 (session : SessionId) :
    Step (race2_s1 session) Event.finishClose (race2_s2 session) := by
  exact Step.finishClose rfl rfl

theorem close_wins_race_rejects_insert (session : SessionId) :
    ∃ (s1 s2 : State),
      Step (race_s0 session) Event.closeRegistry s1 ∧
      Step s1 Event.finishClose s2 ∧
      Reachable (race_s0 session) s2 ∧
      s2.CloseCertified ∧
      (∀ (s' : State), ¬ Step s1 Event.insertFresh s') ∧
      (∀ (gen : Generation) (s' : State), ¬ Step s1 (Event.insertReuse 0 gen) s') ∧
      (∀ (s' : State), ¬ Step s2 Event.insertFresh s') ∧
      (∀ (gen : Generation) (s' : State), ¬ Step s2 (Event.insertReuse 0 gen) s') := by
  refine ⟨race2_s1 session, race2_s2 session, ?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_⟩
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
    rename_i hMay
    cases hMay with
    | inl hO => contradiction
    | inr hDP => exact Nat.lt_irrefl 0 hDP.2
  · intro gen s' hStep
    cases hStep
    rename_i hMay _ _
    cases hMay with
    | inl hO => contradiction
    | inr hDP => exact Nat.lt_irrefl 0 hDP.2
  · intro s' hStep
    cases hStep
    rename_i hMay
    cases hMay with
    | inl hO => contradiction
    | inr hDP => exact Nat.lt_irrefl 0 hDP.2
  · intro gen s' hStep
    cases hStep
    rename_i hMay _ _
    cases hMay with
    | inl hO => contradiction
    | inr hDP => exact Nat.lt_irrefl 0 hDP.2

end XlFnFormal.Handle
