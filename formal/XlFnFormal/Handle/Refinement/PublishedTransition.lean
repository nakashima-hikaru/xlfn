import XlFnFormal.Handle.Refinement.PublishedModel

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Refinement

open XlFnFormal.Handle.Topics

inductive Event where
  | installProvisional (key : TopicKey) (token : Registry.Token) (rtdKey : RtdKey)
  | activatePublication (key : TopicKey) (token : Registry.Token)
  | beginWarmRead (readerId : Nat) (key : TopicKey)
  | finishWarmRead (readerId : Nat)
  | abandonWarmRead (readerId : Nat)
  | invalidatePublication (key : TopicKey) (token : Registry.Token)
  | closePublications
  | registryClose
deriving DecidableEq, Repr

def closingState : PublicationState → PublicationState
  | .provisional => .closing
  | .live => .closing
  | state => state

def apply? (s : State) (event : Event) : Option State :=
  match event with
  | .installProvisional key token rtdKey =>
      if s.findPublication? key token = none ∧
          s.findSnapshot? key = none then
        some { s with
          publications := s.publications ++
            [{ key := key, token := token, rtdKey := rtdKey, state := .provisional }] }
      else none
  | .activatePublication key token =>
      match s.findPublication? key token with
      | some publication =>
          if publication.state = .provisional ∧
              s.findSnapshot? key = none ∧
              s.canonicalTopic? { publication with state := .live } = true then
            some { s with
              publications := s.updatePublication key token .live
              snapshot := s.snapshot ++ [{ key := key, token := token }] }
          else none
      | none => none
  | .beginWarmRead readerId key =>
      match s.findSnapshot? key with
      | some binding =>
          match s.findPublication? binding.key binding.token with
          | some publication =>
              if publication.state = .live ∧
                  s.canonicalTopic? publication = true ∧
                  s.findWarmRead? readerId = none then
                some { s with
                  warmReads := s.warmReads ++
                    [{ id := readerId, key := binding.key, token := binding.token,
                       rtdKey := publication.rtdKey }] }
              else none
          | none => none
      | none => none
  | .finishWarmRead readerId =>
      match s.findWarmRead? readerId with
      | some read =>
          match s.findPublication? read.key read.token with
          | some publication =>
              if publication.state = .live ∧ publication.rtdKey = read.rtdKey then
                some { s with warmReads := s.removeWarmRead readerId }
              else none
          | none => none
      | none => none
  | .abandonWarmRead readerId =>
      match s.findWarmRead? readerId with
      | some read =>
          match s.findPublication? read.key read.token with
          | some publication =>
              if (publication.state = .stale ∨ publication.state = .closing) ∧
                  publication.rtdKey = read.rtdKey then
                some { s with warmReads := s.removeWarmRead readerId }
              else none
          | none => none
      | none => none
  | .invalidatePublication key token =>
      match s.findPublication? key token with
      | some publication =>
          match s.findSnapshot? key with
          | some binding =>
              if publication.state = .live ∧ binding.token = token then
                some { s with
                  publications := s.updatePublication key token .stale
                  snapshot := s.removeSnapshot key }
              else none
          | none => none
      | none => none
  | .closePublications =>
      some { s with
        publications := s.publications.map (fun publication =>
          { publication with state := closingState publication.state })
        snapshot := [] }
  | .registryClose =>
      if s.warmReads = [] then some s else none

def Step (s : State) (event : Event) (s' : State) : Prop :=
  apply? s event = some s'

theorem apply?_sound
    {s s' : State} {event : Event}
    (h : apply? s event = some s') :
    Step s event s' := h

theorem apply?_complete
    {s s' : State} {event : Event}
    (h : Step s event s') :
    apply? s event = some s' := h

end XlFnFormal.Handle.Refinement
