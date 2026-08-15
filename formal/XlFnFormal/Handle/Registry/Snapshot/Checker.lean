import XlFnFormal.Handle.Registry.Snapshot.Transition
import XlFnFormal.Handle.Registry.Checker

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Registry.Snapshot

open XlFnFormal.Handle.Registry

def apply? (s : State) (e : Event) : Option State :=
  match e with
  | .insertFresh =>
    match Registry.apply? s.registry .insertFresh with
    | some reg' =>
        match s.findSnapshot? s.registry.slots.length with
        | none =>
            match s.findPublication? s.registry.slots.length 1 with
            | none =>
                some { s with
                  registry := reg'
                  publications := s.publications ++ [{ slot := s.registry.slots.length, generation := 1, state := .live }]
                  snapshot := s.snapshot ++ [{ slot := s.registry.slots.length, generation := 1 }] }
            | some _ => none
        | some _ => none
    | none => none

  | .insertReuse slot generation =>
    match Registry.apply? s.registry (.insertReuse slot generation) with
    | some reg' =>
        match s.findSnapshot? slot with
        | none =>
            match s.findPublication? slot generation with
            | none =>
                some { s with
                  registry := reg'
                  publications := s.publications ++ [{ slot := slot, generation := generation, state := .live }]
                  snapshot := s.snapshot ++ [{ slot := slot, generation := generation }] }
            | some _ => none
        | some _ => none
    | none => none

  | .removeReuse token nextGen =>
    match Registry.apply? s.registry (.removeReuse token nextGen) with
    | some reg' =>
        match s.findPublication? token.slot token.generation with
        | some pub =>
            match pub.state with
            | .live =>
                some { s with
                  registry := reg'
                  publications := s.updatePublicationState token.slot token.generation .stale
                  snapshot := s.removeSnapshot token.slot }
            | _ => none
        | none => none
    | none => none

  | .removeRetire token =>
    match Registry.apply? s.registry (.removeRetire token) with
    | some reg' =>
        match s.findPublication? token.slot token.generation with
        | some pub =>
            match pub.state with
            | .live =>
                some { s with
                  registry := reg'
                  publications := s.updatePublicationState token.slot token.generation .stale
                  snapshot := s.removeSnapshot token.slot }
            | _ => none
        | none => none
    | none => none

  | .beginFastObservation readerId token =>
    match s.findFastLookup? readerId with
    | none =>
        if token.session = s.registry.session then
          match s.findSnapshot? token.slot with
          | some binding =>
              if binding.generation = token.generation then
                match s.findPublication? token.slot token.generation with
                | some pub =>
                    match pub.state with
                    | .live =>
                        some { s with
                          fastLookups := s.fastLookups ++
                            [{ id := readerId, token := token, stage := .observed }] }
                    | _ => none
                | none => none
              else none
          | none => none
        else none
    | some _ => none

  | .acquireTentativeLease readerId =>
    match s.findFastLookup? readerId with
    | some lookup =>
        if lookup.stage = .observed ∧ s.leaseAdmission ≠ .sealed ∧ s.registry.closed = false then
          some { s with fastLookups := s.updateFastLookupStage readerId .tentative }
        else none
    | none => none

  | .abandonObservation readerId =>
    match s.findFastLookup? readerId with
    | some lookup =>
        if lookup.stage = .observed ∧ s.leaseAdmission ≠ .open then
          some { s with fastLookups := s.removeFastLookup readerId }
        else none
    | none => none

  | .validateFastLookup readerId =>
    match s.findFastLookup? readerId with
    | some lookup =>
        if lookup.stage = .tentative then
          match s.findPublication? lookup.token.slot lookup.token.generation with
          | some pub =>
              match pub.state with
              | .live =>
                  match Registry.apply? s.registry (.beginLookup lookup.token) with
                  | some reg' =>
                      some { s with
                        registry := reg'
                        fastLookups := s.updateFastLookupStage readerId .validated }
                  | none => none
              | _ => none
          | none => none
        else none
    | none => none

  | .rejectTentativeFastLookup readerId =>
    match s.findFastLookup? readerId with
    | some lookup =>
        if lookup.stage = .tentative then
          match s.findPublication? lookup.token.slot lookup.token.generation with
          | some pub =>
              if pub.state ≠ .live then
                some { s with fastLookups := s.removeFastLookup readerId }
              else none
          | none => none
        else none
    | none => none

  | .completeFastLookup readerId =>
    match s.findFastLookup? readerId with
    | some lookup =>
        if lookup.stage = .validated then
          match Registry.apply? s.registry .endLookup with
          | some reg' =>
              some { s with
                registry := reg'
                fastLookups := s.removeFastLookup readerId }
          | none => none
        else none
    | none => none

  | .fallbackFastLookup readerId =>
    match s.findFastLookup? readerId with
    | some lookup =>
        if lookup.stage = .validated then
          match s.findPublication? lookup.token.slot lookup.token.generation with
          | some pub =>
              if pub.state ≠ .live then
                match Registry.apply? s.registry .endLookup with
                | some reg' =>
                    some { s with
                      registry := reg'
                      fastLookups := s.removeFastLookup readerId }
                | none => none
              else none
          | none => none
        else none
    | none => none

  | .beginSlowLookup token =>
    if s.leaseAdmission ≠ .sealed then
      match Registry.apply? s.registry (.beginLookup token) with
      | some reg' => some { s with registry := reg' }
      | none => none
    else none

  | .endSlowLookup =>
    if s.validatedFastLookups.length < s.registry.activeLeases then
      match Registry.apply? s.registry .endLookup with
      | some reg' => some { s with registry := reg' }
      | none => none
    else none

  | .beginSealLeaseAdmission =>
    if s.leaseAdmission = .open then
      some { s with leaseAdmission := .sealing }
    else none

  | .finishSealLeaseAdmission =>
    if s.leaseAdmission = .sealing then
      some { s with leaseAdmission := .sealed }
    else none

  | .closeRegistry =>
    if s.leaseAdmission = .sealed then
      match Registry.apply? s.registry .closeRegistry with
      | some reg' =>
          some { s with
            registry := reg'
            publications := s.updateClosingPublications
            snapshot := [] }
      | none => none
    else none

  | .finishClose =>
    if s.tentativeFastLookups = [] ∧ s.validatedFastLookups = [] then
      match Registry.apply? s.registry .finishClose with
      | some reg' => some s
      | none => none
    else none

theorem apply?_sound
    {s s' : State} {e : Event}
    (h : apply? s e = some s') :
    Step s e s' := by
  cases e with
  | insertFresh =>
      dsimp [apply?] at h
      cases hReg : Registry.apply? s.registry .insertFresh with
      | none => rw [hReg] at h; contradiction
      | some reg' =>
          rw [hReg] at h
          dsimp at h
          cases hSnap : s.findSnapshot? s.registry.slots.length with
          | some _ => rw [hSnap] at h; contradiction
          | none =>
              rw [hSnap] at h
              dsimp at h
              cases hPub : s.findPublication? s.registry.slots.length 1 with
              | some _ => rw [hPub] at h; contradiction
              | none =>
                  rw [hPub] at h
                  dsimp at h
                  cases h
                  exact Step.insertFresh (Registry.apply?_sound hReg) hSnap hPub
  | insertReuse slot gen =>
      dsimp [apply?] at h
      cases hReg : Registry.apply? s.registry (.insertReuse slot gen) with
      | none => rw [hReg] at h; contradiction
      | some reg' =>
          rw [hReg] at h
          dsimp at h
          cases hSnap : s.findSnapshot? slot with
          | some _ => rw [hSnap] at h; contradiction
          | none =>
              rw [hSnap] at h
              dsimp at h
              cases hPub : s.findPublication? slot gen with
              | some _ => rw [hPub] at h; contradiction
              | none =>
                  rw [hPub] at h
                  dsimp at h
                  cases h
                  exact Step.insertReuse (Registry.apply?_sound hReg) hSnap hPub
  | removeReuse token nextGen =>
      dsimp [apply?] at h
      cases hReg : Registry.apply? s.registry (.removeReuse token nextGen) with
      | none => rw [hReg] at h; contradiction
      | some reg' =>
          rw [hReg] at h
          dsimp at h
          cases hPub : s.findPublication? token.slot token.generation with
          | none => rw [hPub] at h; contradiction
          | some pub =>
              rw [hPub] at h
              dsimp at h
              cases hLive : pub.state with
              | live =>
                  rw [hLive] at h
                  dsimp at h
                  cases h
                  exact Step.removeReuse (Registry.apply?_sound hReg) hPub hLive
              | closing => rw [hLive] at h; contradiction
              | stale => rw [hLive] at h; contradiction
  | removeRetire token =>
      dsimp [apply?] at h
      cases hReg : Registry.apply? s.registry (.removeRetire token) with
      | none => rw [hReg] at h; contradiction
      | some reg' =>
          rw [hReg] at h
          dsimp at h
          cases hPub : s.findPublication? token.slot token.generation with
          | none => rw [hPub] at h; contradiction
          | some pub =>
              rw [hPub] at h
              dsimp at h
              cases hLive : pub.state with
              | live =>
                  rw [hLive] at h
                  dsimp at h
                  cases h
                  exact Step.removeRetire (Registry.apply?_sound hReg) hPub hLive
              | closing => rw [hLive] at h; contradiction
              | stale => rw [hLive] at h; contradiction
  | beginFastObservation readerId token =>
      dsimp [apply?] at h
      cases hNoReader : s.findFastLookup? readerId with
      | some _ => rw [hNoReader] at h; contradiction
      | none =>
          rw [hNoReader] at h
          dsimp at h
          by_cases hAuth : token.session = s.registry.session
          · rw [if_pos hAuth] at h
            cases hSnap : s.findSnapshot? token.slot with
            | none => rw [hSnap] at h; contradiction
            | some binding =>
                rw [hSnap] at h
                dsimp at h
                by_cases hSnapGen : binding.generation = token.generation
                · rw [if_pos hSnapGen] at h
                  cases hPub : s.findPublication? token.slot token.generation with
                  | none => rw [hPub] at h; contradiction
                  | some pub =>
                      rw [hPub] at h
                      dsimp at h
                      cases hLive : pub.state with
                      | live =>
                          rw [hLive] at h
                          dsimp at h
                          cases h
                          exact Step.beginFastObservation hNoReader hSnap hSnapGen hPub hAuth hLive
                      | closing => rw [hLive] at h; contradiction
                      | stale => rw [hLive] at h; contradiction
                · rw [if_neg hSnapGen] at h; contradiction
          · rw [if_neg hAuth] at h; contradiction
  | acquireTentativeLease readerId =>
      dsimp [apply?] at h
      cases hLookup : s.findFastLookup? readerId with
      | none => rw [hLookup] at h; contradiction
      | some lookup =>
          rw [hLookup] at h
          dsimp at h
          by_cases hCond : lookup.stage = .observed ∧ s.leaseAdmission ≠ .sealed ∧ s.registry.closed = false
          · rw [if_pos hCond] at h
            cases h
            exact Step.acquireTentativeLease hLookup hCond.1 hCond.2.1 hCond.2.2
          · rw [if_neg hCond] at h; contradiction
  | abandonObservation readerId =>
      dsimp [apply?] at h
      cases hLookup : s.findFastLookup? readerId with
      | none => rw [hLookup] at h; contradiction
      | some lookup =>
          rw [hLookup] at h
          dsimp at h
          by_cases hCond : lookup.stage = .observed ∧ s.leaseAdmission ≠ .open
          · rw [if_pos hCond] at h
            cases h
            exact Step.abandonObservation hLookup hCond.1 hCond.2
          · rw [if_neg hCond] at h; contradiction
  | validateFastLookup readerId =>
      dsimp [apply?] at h
      cases hLookup : s.findFastLookup? readerId with
      | none => rw [hLookup] at h; contradiction
      | some lookup =>
          rw [hLookup] at h
          dsimp at h
          cases hTentative : lookup.stage with
          | observed => rw [hTentative] at h; contradiction
          | tentative =>
              rw [hTentative] at h
              dsimp at h
              cases hPub : s.findPublication? lookup.token.slot lookup.token.generation with
              | none => rw [hPub] at h; contradiction
              | some pub =>
                  rw [hPub] at h
                  dsimp at h
                  cases hLive : pub.state with
                  | live =>
                      rw [hLive] at h
                      dsimp at h
                      cases hReg : Registry.apply? s.registry (.beginLookup lookup.token) with
                      | none => rw [hReg] at h; contradiction
                      | some reg' =>
                          rw [hReg] at h
                          dsimp at h
                          cases h
                          exact Step.validateFastLookup hLookup hTentative hPub hLive (Registry.apply?_sound hReg)
                  | closing => rw [hLive] at h; contradiction
                  | stale => rw [hLive] at h; contradiction
          | validated => rw [hTentative] at h; contradiction
  | rejectTentativeFastLookup readerId =>
      dsimp [apply?] at h
      cases hLookup : s.findFastLookup? readerId with
      | none => rw [hLookup] at h; contradiction
      | some lookup =>
          rw [hLookup] at h
          dsimp at h
          cases hTentative : lookup.stage with
          | observed => rw [hTentative] at h; contradiction
          | tentative =>
              rw [hTentative] at h
              dsimp at h
              cases hPub : s.findPublication? lookup.token.slot lookup.token.generation with
              | none => rw [hPub] at h; contradiction
              | some pub =>
                  rw [hPub] at h
                  dsimp at h
                  by_cases hNotLive : pub.state ≠ .live
                  · rw [if_pos hNotLive] at h
                    cases h
                    exact Step.rejectTentativeFastLookup hLookup hTentative hPub hNotLive
                  · rw [if_neg hNotLive] at h; contradiction
          | validated => rw [hTentative] at h; contradiction
  | completeFastLookup readerId =>
      dsimp [apply?] at h
      cases hLookup : s.findFastLookup? readerId with
      | none => rw [hLookup] at h; contradiction
      | some lookup =>
          rw [hLookup] at h
          dsimp at h
          cases hValidated : lookup.stage with
          | observed => rw [hValidated] at h; contradiction
          | tentative => rw [hValidated] at h; contradiction
          | validated =>
              rw [hValidated] at h
              dsimp at h
              cases hReg : Registry.apply? s.registry .endLookup with
              | none => rw [hReg] at h; contradiction
              | some reg' =>
                  rw [hReg] at h
                  dsimp at h
                  cases h
                  exact Step.completeFastLookup hLookup hValidated (Registry.apply?_sound hReg)
  | fallbackFastLookup readerId =>
      dsimp [apply?] at h
      cases hLookup : s.findFastLookup? readerId with
      | none => rw [hLookup] at h; contradiction
      | some lookup =>
          rw [hLookup] at h
          dsimp at h
          cases hValidated : lookup.stage with
          | observed => rw [hValidated] at h; contradiction
          | tentative => rw [hValidated] at h; contradiction
          | validated =>
              rw [hValidated] at h
              dsimp at h
              cases hPub : s.findPublication? lookup.token.slot lookup.token.generation with
              | none => rw [hPub] at h; contradiction
              | some pub =>
                  rw [hPub] at h
                  dsimp at h
                  by_cases hNotLive : pub.state ≠ .live
                  · rw [if_pos hNotLive] at h
                    cases hReg : Registry.apply? s.registry .endLookup with
                    | none => rw [hReg] at h; contradiction
                    | some reg' =>
                        rw [hReg] at h
                        dsimp at h
                        cases h
                        exact Step.fallbackFastLookup hLookup hValidated hPub hNotLive (Registry.apply?_sound hReg)
                  · rw [if_neg hNotLive] at h; contradiction
  | beginSlowLookup token =>
      dsimp [apply?] at h
      by_cases hNotSealed : s.leaseAdmission ≠ .sealed
      · rw [if_pos hNotSealed] at h
        cases hReg : Registry.apply? s.registry (.beginLookup token) with
        | none => rw [hReg] at h; contradiction
        | some reg' =>
            rw [hReg] at h
            dsimp at h
            cases h
            exact Step.beginSlowLookup hNotSealed (Registry.apply?_sound hReg)
      · rw [if_neg hNotSealed] at h; contradiction
  | endSlowLookup =>
      dsimp [apply?] at h
      dsimp [State.validatedFastLookups] at h
      split at h
      · rename_i hSlowLease
        cases hReg : Registry.apply? s.registry .endLookup with
        | none => rw [hReg] at h; contradiction
        | some reg' =>
            rw [hReg] at h
            dsimp at h
            cases h
            exact Step.endSlowLookup hSlowLease (Registry.apply?_sound hReg)
      · contradiction
  | beginSealLeaseAdmission =>
      dsimp [apply?] at h
      by_cases hOpen : s.leaseAdmission = .open
      · rw [if_pos hOpen] at h
        cases h
        exact Step.beginSealLeaseAdmission hOpen
      · rw [if_neg hOpen] at h; contradiction
  | finishSealLeaseAdmission =>
      dsimp [apply?] at h
      by_cases hSealing : s.leaseAdmission = .sealing
      · rw [if_pos hSealing] at h
        cases h
        exact Step.finishSealLeaseAdmission hSealing
      · rw [if_neg hSealing] at h; contradiction
  | closeRegistry =>
      dsimp [apply?] at h
      by_cases hSealed : s.leaseAdmission = .sealed
      · rw [if_pos hSealed] at h
        cases hReg : Registry.apply? s.registry .closeRegistry with
        | none => rw [hReg] at h; contradiction
        | some reg' =>
            rw [hReg] at h
            dsimp at h
            cases h
            exact Step.closeRegistry hSealed (Registry.apply?_sound hReg)
      · rw [if_neg hSealed] at h; contradiction
  | finishClose =>
      dsimp [apply?] at h
      split at h
      · rename_i hNoFast
        cases hReg : Registry.apply? s.registry .finishClose with
        | none => rw [hReg] at h; contradiction
        | some reg' =>
            rw [hReg] at h
            dsimp at h
            cases h
            exact Step.finishClose hNoFast.1 hNoFast.2 (Registry.apply?_sound hReg)
      · contradiction

theorem apply?_complete
    {s s' : State} {e : Event}
    (h : Step s e s') :
    apply? s e = some s' := by
  cases h with
  | insertFresh hReg hNoSnap hNoPub =>
      have hRegApp := Registry.apply?_complete hReg
      dsimp [apply?]
      rw [hRegApp]
      dsimp
      rw [hNoSnap, hNoPub]
  | insertReuse hReg hNoSnap hNoPub =>
      have hRegApp := Registry.apply?_complete hReg
      dsimp [apply?]
      rw [hRegApp]
      dsimp
      rw [hNoSnap, hNoPub]
  | removeReuse hReg hPub hLive =>
      have hRegApp := Registry.apply?_complete hReg
      dsimp [apply?]
      rw [hRegApp]
      dsimp
      rw [hPub]
      dsimp
      rw [hLive]
  | removeRetire hReg hPub hLive =>
      have hRegApp := Registry.apply?_complete hReg
      dsimp [apply?]
      rw [hRegApp]
      dsimp
      rw [hPub]
      dsimp
      rw [hLive]
  | beginFastObservation hNoReader hSnap hSnapGen hPub hAuth hLive =>
      dsimp [apply?]
      rw [hNoReader]
      dsimp
      rw [hAuth]
      rw [hSnap]
      dsimp
      rw [if_pos hSnapGen, hPub]
      dsimp
      simp [hLive]
  | acquireTentativeLease hLookup hObs hNotSealed hNotClosed =>
      dsimp [apply?]
      rw [hLookup]
      dsimp
      simp [hObs, hNotSealed, hNotClosed]
  | abandonObservation hLookup hObs hNotOpen =>
      dsimp [apply?]
      rw [hLookup]
      dsimp
      simp [hObs, hNotOpen]
  | validateFastLookup hLookup hTentative hPub hLive hReg =>
      have hRegApp := Registry.apply?_complete hReg
      dsimp [apply?]
      rw [hLookup]
      dsimp
      simp [hTentative, hPub, hLive, hRegApp]
  | rejectTentativeFastLookup hLookup hTentative hPub hNotLive =>
      dsimp [apply?]
      rw [hLookup]
      dsimp
      simp [hTentative, hPub, hNotLive]
  | completeFastLookup hLookup hValidated hReg =>
      have hRegApp := Registry.apply?_complete hReg
      dsimp [apply?]
      rw [hLookup]
      dsimp
      simp [hValidated, hRegApp]
  | fallbackFastLookup hLookup hValidated hPub hNotLive hReg =>
      have hRegApp := Registry.apply?_complete hReg
      dsimp [apply?]
      rw [hLookup]
      dsimp
      simp [hValidated, hPub, hNotLive, hRegApp]
  | beginSlowLookup hNotSealed hReg =>
      have hRegApp := Registry.apply?_complete hReg
      dsimp [apply?]
      rw [if_pos hNotSealed, hRegApp]
  | endSlowLookup hSlowLease hReg =>
      have hRegApp := Registry.apply?_complete hReg
      dsimp [apply?]
      rw [if_pos hSlowLease, hRegApp]
  | beginSealLeaseAdmission hOpen =>
      dsimp [apply?]
      rw [if_pos hOpen]
  | finishSealLeaseAdmission hSealing =>
      dsimp [apply?]
      rw [if_pos hSealing]
  | closeRegistry hSealed hReg =>
      have hRegApp := Registry.apply?_complete hReg
      dsimp [apply?]
      rw [if_pos hSealed, hRegApp]
  | finishClose hNoTentative hNoValidated hReg =>
      have hRegApp := Registry.apply?_complete hReg
      dsimp [apply?]
      rw [if_pos ⟨hNoTentative, hNoValidated⟩]
      rw [hRegApp]

end XlFnFormal.Handle.Registry.Snapshot
