import Std

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.Rtd.ServerGeneration

abbrev ServerGeneration := Nat

def maxGeneration : ServerGeneration := 2 ^ 64 - 1

structure State where
  last : ServerGeneration
deriving DecidableEq, Repr

def initialState : State :=
  { last := 0 }

def allocate? (s : State) : Option (ServerGeneration × State) :=
  if s.last < maxGeneration then
    let generation := s.last + 1
    some (generation, { last := generation })
  else
    none

end XlFnFormal.Rtd.ServerGeneration
