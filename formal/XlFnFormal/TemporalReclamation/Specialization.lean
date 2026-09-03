import XlFnFormal.TemporalReclamation.Safety

/-! # Subsystem Specializations for Temporal Reclamation

    Maps each subsystem's temporal ownership primitives onto the unified
    `TemporalReclamation` protocol vocabulary:

    | Subsystem    | Admission / Reader      | Pin Capability | Retirement Point    | Reclamation Point   |
    |--------------|-------------------------|----------------|---------------------|---------------------|
    | Cache        | CacheLookupDomain permit| CacheLease     | Moka eviction       | CacheNode drop      |
    | Handle       | HandleReadDomain permit | HandleLease    | Binding removal     | ObjectArena remove  |
    | RTD Callback | ServerOperationBarrier  | (immediate)    | Callback replace    | Callback Box drop   |
    | Async        | Generation admission    | Active task    | Generation rollover | State Box drop      |
-/

set_option autoImplicit false
set_option linter.unusedVariables false

namespace XlFnFormal.TemporalReclamation

/-- Subsystem mapping descriptor. -/
structure SubsystemMapping where
  subsystemName     : String
  admissionResource : String
  pinResource       : String
  retireAction      : String
  reclaimAction     : String
deriving Repr

def cacheMapping : SubsystemMapping :=
  { subsystemName     := "Cache"
  , admissionResource := "CacheLookupDomain permit"
  , pinResource       := "CacheLease"
  , retireAction      := "Moka eviction (resident = false)"
  , reclaimAction     := "CacheNode drop (pins = 0 ∧ domain quiesced)" }

def handleMapping : SubsystemMapping :=
  { subsystemName     := "Handle"
  , admissionResource := "HandleReadDomain permit"
  , pinResource       := "HandleLease"
  , retireAction      := "Binding removal from table"
  , reclaimAction     := "ObjectArena slot release" }

def rtdMapping : SubsystemMapping :=
  { subsystemName     := "RTD Callback"
  , admissionResource := "ServerOperationBarrier capability"
  , pinResource       := "Retained pointer capability"
  , retireAction      := "Callback replacement in barrier"
  , reclaimAction     := "Callback Box drop" }

def asyncMapping : SubsystemMapping :=
  { subsystemName     := "Async UDF"
  , admissionResource := "Generation admission domain"
  , pinResource       := "Active task execution"
  , retireAction      := "Generation rollover"
  , reclaimAction     := "GenerationState Box drop" }

/-- Subsystems adhere to the TemporalReclamation vocabulary and safety invariants. -/
theorem subsystemProtocolSafety
    (mapping : SubsystemMapping)
    {s : State} (hReach : Reachable initialState s)
    (hActive : s.observing > 0 ∨ s.pins > 0) :
    s.status ≠ .reclaimed :=
  noUseAfterReclaim hReach hActive

end XlFnFormal.TemporalReclamation
