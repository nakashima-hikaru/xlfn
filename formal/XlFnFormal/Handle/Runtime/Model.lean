import XlFnFormal.Handle.Registry.Model

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Handle.Runtime

open Registry (SessionId SlotId Generation Token SlotState closeSlot maxGeneration nextGeneration?)

abbrev InitializerId := Nat

inductive InitializerStage where
  | beforeInsert
  | pending (token : Token)
  | resolved
deriving DecidableEq, Repr

structure Initializer where
  id : InitializerId
  stage : InitializerStage
deriving DecidableEq, Repr

inductive Phase where
  | «open»
  | drainingPrepares
  | registryClosed
  | closed
deriving DecidableEq, Repr

structure State where
  phase : Phase
  registry : Registry.State
  activePrepares : Nat
  initializers : List Initializer
deriving DecidableEq, Repr

def initialState (session : SessionId) : State :=
  { phase := .«open»
    registry := Registry.initialState session
    activePrepares := 0
    initializers := [] }

def State.findInitializer? (s : State) (id : InitializerId) : Option Initializer :=
  s.initializers.find? (fun i => i.id == id)

def State.removeInitializer (s : State) (id : InitializerId) : List Initializer :=
  s.initializers.filter (fun i => i.id != id)

def State.updateInitializer (s : State) (id : InitializerId) (newStage : InitializerStage) : List Initializer :=
  s.initializers.map (fun i => if i.id == id then { i with stage := newStage } else i)

def TokenLive (reg : Registry.State) (token : Token) : Prop :=
  token.session = reg.session ∧
  ∃ h : token.slot < reg.slots.length,
    reg.slots.get ⟨token.slot, h⟩ = .live token.generation

end XlFnFormal.Handle.Runtime
