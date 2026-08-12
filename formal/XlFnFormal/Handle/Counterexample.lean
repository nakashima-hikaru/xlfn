import XlFnFormal.Handle.Safety

set_option autoImplicit false

namespace XlFnFormal.Handle

def race_s0 (session : SessionId) : State := State.initialState session 1

def race1_s1 (session : SessionId) : State :=
  { session := session, phase := .«open», slots := [.live 0], activePrepares := 0, activeLeases := 0 }

def race1_s2 (session : SessionId) : State :=
  { session := session, phase := .registryClosed, slots := [.vacant 1], activePrepares := 0, activeLeases := 0 }

def race1_s3 (session : SessionId) : State :=
  { session := session, phase := .closed, slots := [.vacant 1], activePrepares := 0, activeLeases := 0 }

theorem Step_race1_step1 (session : SessionId) :
    Step (race_s0 session) (Event.insert 0 0) (race1_s1 session) := by
  have hInBounds : 0 < (race_s0 session).slots.length := by dsimp [race_s0, State.initialState]; decide
  have hVacant : (race_s0 session).slots.get ⟨0, hInBounds⟩ = SlotState.vacant 0 := rfl
  exact Step.insert (Or.inl rfl) hInBounds hVacant

theorem Step_race1_step2 (session : SessionId) :
    Step (race1_s1 session) Event.closeRegistry (race1_s2 session) := by
  exact Step.closeRegistry (Or.inl rfl) rfl

theorem Step_race1_step3 (session : SessionId) :
    Step (race1_s2 session) Event.finishClose (race1_s3 session) := by
  exact Step.finishClose rfl rfl

theorem insert_wins_race_is_quiescent (session : SessionId) :
    ∃ (s1 s2 s3 : State),
      Step (race_s0 session) (Event.insert 0 0) s1 ∧
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
  · refine ⟨rfl, rfl, rfl, ?_⟩
    intro slot hMem
    rcases List.mem_singleton.mp hMem with rfl
    exact trivial

def race2_s1 (session : SessionId) : State :=
  { session := session, phase := .registryClosed, slots := [.vacant 0], activePrepares := 0, activeLeases := 0 }

def race2_s2 (session : SessionId) : State :=
  { session := session, phase := .closed, slots := [.vacant 0], activePrepares := 0, activeLeases := 0 }

theorem Step_race2_step1 (session : SessionId) :
    Step (race_s0 session) Event.closeRegistry (race2_s1 session) := by
  have hPhase : (race_s0 session).phase = .«open» ∨ (race_s0 session).phase = .drainingPrepares := Or.inl rfl
  have hNoPrepares : (race_s0 session).activePrepares = 0 := rfl
  exact Step.closeRegistry hPhase hNoPrepares

theorem Step_race2_step2 (session : SessionId) :
    Step (race2_s1 session) Event.finishClose (race2_s2 session) := by
  exact Step.finishClose rfl rfl

theorem close_wins_race_rejects_insert (session : SessionId) :
    ∃ (s1 s2 : State),
      Step (race_s0 session) Event.closeRegistry s1 ∧
      Step s1 Event.finishClose s2 ∧
      Reachable (race_s0 session) s2 ∧
      s2.CloseCertified ∧
      (∀ (s' : State), ¬ Step s1 (Event.insert 0 0) s') ∧
      (∀ (s' : State), ¬ Step s2 (Event.insert 0 0) s') := by
  refine ⟨race2_s1 session, race2_s2 session, ?_, ?_, ?_, ?_, ?_, ?_⟩
  · exact Step_race2_step1 session
  · exact Step_race2_step2 session
  · exact Reachable.step (Reachable.step Reachable.init
      (Step_race2_step1 session))
      (Step_race2_step2 session)
  · refine ⟨rfl, rfl, rfl, ?_⟩
    intro slot hMem
    rcases List.mem_singleton.mp hMem with rfl
    exact trivial
  · intro s' hStep
    cases hStep
    rename_i hPhase _ _
    cases hPhase with
    | inl hO => contradiction
    | inr hDP => contradiction
  · intro s' hStep
    cases hStep
    rename_i hPhase _ _
    cases hPhase with
    | inl hO => contradiction
    | inr hDP => contradiction

end XlFnFormal.Handle
