import XlFnFormal.Handle.Registry.Checker

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Registry

theorem trace_insert_lookup_close_completes
    (session : SessionId) :
    let s0 := initialState session
    let token : Token := { session := session, slot := 0, generation := 1 }
    let s1 := { s0 with slots := [.live 1] }
    let s2 := { s1 with activeLeases := 1 }
    let s3 := { s2 with activeLeases := 0 }
    let s4 := { s3 with closed := true, slots := [.vacant 1] }
    Step s0 .insertFresh s1 ∧
    Step s1 (.beginLookup token) s2 ∧
    Step s2 .endLookup s3 ∧
    Step s3 .closeRegistry s4 ∧
    Step s4 .finishClose s4 := by
  intro s0 token s1 s2 s3 s4
  have h0 : Step s0 .insertFresh s1 := Step.insertFresh (by dsimp [s0, initialState, State.MayInsert])
  have hInBounds1 : token.slot < s1.slots.length := by dsimp [token, s1, s0, initialState]; decide
  have hLive1 : s1.slots.get ⟨token.slot, hInBounds1⟩ = .live token.generation := by rfl
  have hAuth1 : s1.AuthenticatedFor token := by rfl
  have h1 : Step s1 (.beginLookup token) s2 := Step.beginLookup (by dsimp [s1, s0, initialState]) hAuth1 hInBounds1 hLive1
  have h2 : Step s2 .endLookup s3 := Step.endLookup (by dsimp [s2, s1, s0, initialState]; decide)
  have h3 : Step s3 .closeRegistry s4 := Step.closeRegistry (by dsimp [s3, s2, s1, s0, initialState])
  have h4 : Step s4 .finishClose s4 := Step.finishClose (by dsimp [s4, s3, s2, s1, s0, initialState]) (by dsimp [s4, s3, s2, s1, s0, initialState])
  exact ⟨h0, h1, h2, h3, h4⟩

end XlFnFormal.Handle.Registry
