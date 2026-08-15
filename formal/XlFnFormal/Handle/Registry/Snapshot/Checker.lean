import XlFnFormal.Handle.Registry.Snapshot.Transition
import XlFnFormal.Handle.Registry.Checker

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Registry.Snapshot

open XlFnFormal.Handle.Registry

/-! Executable checker for the RCU publication protocol.  `observeBorrow` is
    the successful `Live` observation and borrow linearization point. -/

def apply? (s : State) (e : Event) : Option State :=
  match e with
  | .insertFresh =>
      match Registry.apply? s.registry .insertFresh with
      | some reg' =>
          if s.findSnapshot? s.registry.slots.length = none then
            if s.findPublication? s.registry.slots.length 1 = none then
              some { s with
                registry := reg'
                publications := s.publications ++
                  [{ slot := s.registry.slots.length, generation := 1, state := .live }]
                snapshot := s.snapshot ++
                  [{ slot := s.registry.slots.length, generation := 1 }] }
            else none
          else none
      | none => none
  | .insertReuse slot generation =>
      match Registry.apply? s.registry (.insertReuse slot generation) with
      | some reg' =>
          if s.findSnapshot? slot = none then
            if s.findPublication? slot generation = none then
              some { s with
                registry := reg'
                publications := s.publications ++
                  [{ slot := slot, generation := generation, state := .live }]
                snapshot := s.snapshot ++ [{ slot := slot, generation := generation }] }
            else none
          else none
      | none => none
  | .removeReuse token nextGeneration =>
      match Registry.apply? s.registry (.removeReuse token nextGeneration) with
      | some reg' =>
          match s.findPublication? token.slot token.generation with
          | some pub =>
              if pub.state = .live then
                some { s with
                  registry := reg'
                  publications := s.updatePublicationState
                    token.slot token.generation .stale
                  snapshot := s.removeSnapshot token.slot }
              else none
          | none => none
      | none => none
  | .removeRetire token =>
      match Registry.apply? s.registry (.removeRetire token) with
      | some reg' =>
          match s.findPublication? token.slot token.generation with
          | some pub =>
              if pub.state = .live then
                some { s with
                  registry := reg'
                  publications := s.updatePublicationState
                    token.slot token.generation .stale
                  snapshot := s.removeSnapshot token.slot }
              else none
          | none => none
      | none => none
  | .observeBorrow readerId token =>
      match Registry.apply? s.registry (.beginLookup token) with
      | some reg' =>
          if s.findBorrow? readerId = none then
            match s.findSnapshot? token.slot with
            | some binding =>
                if binding.generation == token.generation then
                  match s.findPublication? token.slot token.generation with
                  | some pub =>
                      if token.session == s.registry.session ∧ pub.state = .live then
                        some { s with
                          registry := reg'
                          borrows := s.borrows ++ [{ id := readerId, token := token }] }
                      else none
                  | none => none
                else none
            | none => none
          else none
      | none => none
  | .releaseBorrow readerId =>
      match Registry.apply? s.registry .endLookup with
      | some reg' =>
          if s.findBorrow? readerId ≠ none then
            some { s with
              registry := reg'
              borrows := s.borrows.filter (fun b => b.id != readerId) }
          else none
      | none => none
  | .retirePublication slot generation =>
      match s.findPublication? slot generation with
      | some pub =>
          if pub.state ≠ .live ∧
              s.findSnapshot? slot = none ∧
              s.findBorrowFor? slot generation = none then
            some { s with publications := s.removePublication slot generation }
          else none
      | none => none
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
      | some reg' =>
          if s.borrows = [] ∧ s.publications = [] ∧ s.snapshot = [] then
            some { s with registry := reg' }
          else none
      | none => none

def accepts (s : State) (e : Event) : Bool :=
  (apply? s e).isSome

end XlFnFormal.Handle.Registry.Snapshot
