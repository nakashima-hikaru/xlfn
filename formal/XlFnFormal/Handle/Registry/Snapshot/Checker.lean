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

  | .beginFastLookup readerId token =>
      match s.findFastLookup? readerId with
      | none =>
          match s.findSnapshot? token.slot with
          | some binding =>
              if binding.generation = token.generation then
                match s.findPublication? token.slot token.generation with
                | some pub =>
                    match pub.state with
                    | .live =>
                        match Registry.apply? s.registry (.beginLookup token) with
                        | some reg' =>
                            some { s with
                              registry := reg'
                              fastLookups := s.fastLookups ++ [{ id := readerId, token := token }] }
                        | none => none
                    | _ => none
                | none => none
              else none
          | none => none
      | some _ => none

  | .completeFastLookup readerId =>
      match s.findFastLookup? readerId with
      | some lookup =>
          match Registry.apply? s.registry .endLookup with
          | some reg' =>
              some { s with
                registry := reg'
                fastLookups := s.removeFastLookup readerId }
          | none => none
      | none => none

  | .fallbackFastLookup readerId =>
      match s.findFastLookup? readerId with
      | some lookup =>
          match Registry.apply? s.registry .endLookup with
          | some reg' =>
              some { s with
                registry := reg'
                fastLookups := s.removeFastLookup readerId }
          | none => none
      | none => none

  | .beginSlowLookup token =>
      match Registry.apply? s.registry (.beginLookup token) with
      | some reg' => some { s with registry := reg' }
      | none => none

  | .endSlowLookup =>
      if s.fastLookups.length < s.registry.activeLeases then
        match Registry.apply? s.registry .endLookup with
        | some reg' => some { s with registry := reg' }
        | none => none
      else none

  | .closeRegistry =>
      match Registry.apply? s.registry .closeRegistry with
      | some reg' =>
          some { s with
            registry := reg'
            publications := s.updateClosingPublications
            snapshot := [] }
      | none => none

  | .finishClose =>
      match Registry.apply? s.registry .finishClose with
      | some reg' => some s
      | none => none

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
  | beginFastLookup readerId token =>
      dsimp [apply?] at h
      cases hNoReader : s.findFastLookup? readerId with
      | some _ => rw [hNoReader] at h; contradiction
      | none =>
          rw [hNoReader] at h
          dsimp at h
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
                        cases hReg : Registry.apply? s.registry (.beginLookup token) with
                        | none => rw [hReg] at h; contradiction
                        | some reg' =>
                            rw [hReg] at h
                            dsimp at h
                            cases h
                            exact Step.beginFastLookup hNoReader hSnap hSnapGen hPub hLive (Registry.apply?_sound hReg)
                    | closing => rw [hLive] at h; contradiction
                    | stale => rw [hLive] at h; contradiction
              · rw [if_neg hSnapGen] at h; contradiction
  | completeFastLookup readerId =>
      dsimp [apply?] at h
      cases hLookup : s.findFastLookup? readerId with
      | none => rw [hLookup] at h; contradiction
      | some lookup =>
          rw [hLookup] at h
          dsimp at h
          cases hReg : Registry.apply? s.registry .endLookup with
          | none => rw [hReg] at h; contradiction
          | some reg' =>
              rw [hReg] at h
              dsimp at h
              cases h
              exact Step.completeFastLookup hLookup (Registry.apply?_sound hReg)
  | fallbackFastLookup readerId =>
      dsimp [apply?] at h
      cases hLookup : s.findFastLookup? readerId with
      | none => rw [hLookup] at h; contradiction
      | some lookup =>
          rw [hLookup] at h
          dsimp at h
          cases hReg : Registry.apply? s.registry .endLookup with
          | none => rw [hReg] at h; contradiction
          | some reg' =>
              rw [hReg] at h
              dsimp at h
              cases h
              exact Step.fallbackFastLookup hLookup (Registry.apply?_sound hReg)
  | beginSlowLookup token =>
      dsimp [apply?] at h
      cases hReg : Registry.apply? s.registry (.beginLookup token) with
      | none => rw [hReg] at h; contradiction
      | some reg' =>
          rw [hReg] at h
          dsimp at h
          cases h
          exact Step.beginSlowLookup (Registry.apply?_sound hReg)
  | endSlowLookup =>
      dsimp [apply?] at h
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
  | closeRegistry =>
      dsimp [apply?] at h
      cases hReg : Registry.apply? s.registry .closeRegistry with
      | none => rw [hReg] at h; contradiction
      | some reg' =>
          rw [hReg] at h
          dsimp at h
          cases h
          exact Step.closeRegistry (Registry.apply?_sound hReg)
  | finishClose =>
      dsimp [apply?] at h
      cases hReg : Registry.apply? s.registry .finishClose with
      | none => rw [hReg] at h; contradiction
      | some reg' =>
          rw [hReg] at h
          dsimp at h
          cases h
          exact Step.finishClose (Registry.apply?_sound hReg)

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
  | beginFastLookup hNoReader hSnap hSnapGen hPub hLive hReg =>
      have hRegApp := Registry.apply?_complete hReg
      dsimp [apply?]
      rw [hNoReader, hSnap]
      dsimp
      rw [if_pos hSnapGen, hPub]
      dsimp
      rw [hLive, hRegApp]
  | completeFastLookup hLookup hReg =>
      have hRegApp := Registry.apply?_complete hReg
      dsimp [apply?]
      rw [hLookup, hRegApp]
  | fallbackFastLookup hLookup hReg =>
      have hRegApp := Registry.apply?_complete hReg
      dsimp [apply?]
      rw [hLookup, hRegApp]
  | beginSlowLookup hReg =>
      have hRegApp := Registry.apply?_complete hReg
      dsimp [apply?]
      rw [hRegApp]
  | endSlowLookup hSlowLease hReg =>
      have hRegApp := Registry.apply?_complete hReg
      dsimp [apply?]
      rw [if_pos hSlowLease, hRegApp]
  | closeRegistry hReg =>
      have hRegApp := Registry.apply?_complete hReg
      dsimp [apply?]
      rw [hRegApp]
  | finishClose hReg =>
      have hRegApp := Registry.apply?_complete hReg
      dsimp [apply?]
      rw [hRegApp]

end XlFnFormal.Handle.Registry.Snapshot
