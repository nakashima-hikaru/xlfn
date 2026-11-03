import XlFnFormal.Shutdown.Safety

set_option autoImplicit false

namespace XlFnFormal.Shutdown

/-- A state similar to an implementation that reports a cleanup error but
unconditionally calls `finish_removal`: one active call still owns module code. -/
def unsafePreFinalState : State :=
  { phase := .closing .finalize,
    resources :=
      { activeCalls := 1,
        generationOwnedByRuntime := false } }

/-- Deliberately unsafe implementation operation, excluded from `Step`. -/
def uncheckedFinish (s : State) : State :=
  { s with phase := .closed }

/-- Merely assigning the `closed` phase does not make a state quiescent. -/
theorem unchecked_finish_is_not_safe :
    (uncheckedFinish unsafePreFinalState).phase = .closed ∧
    ¬ (uncheckedFinish unsafePreFinalState).resources.Quiescent := by
  constructor
  · rfl
  · simp [uncheckedFinish, unsafePreFinalState, Resources.Quiescent,
      Resources.HostDetached, Resources.CallsDrained]

/-- The unsafe operation cannot be represented by the certified transition
relation.  This is the regression property that rules out unconditional
`Runtime::finish_removal()`. -/
theorem unchecked_finish_has_no_certificate :
    ¬ Step unsafePreFinalState .finishClose
      (uncheckedFinish unsafePreFinalState) := by
  apply nonquiescent_cannot_finish
  simp [unsafePreFinalState, Resources.Quiescent,
    Resources.HostDetached, Resources.CallsDrained]


/-- A UDF may have released its `CallGuard` while Excel still owns the
DLL-allocated result that will later be passed to `xlAutoFree12`. -/
def outstandingReturnPreFinalState : State :=
  { phase := .closing .finalize,
    resources :=
      { returnBlocks := 1,
        generationOwnedByRuntime := false } }

/-- Draining calls alone is insufficient: an outstanding return block keeps an
entry point and Rust allocation owned by the module live. -/
theorem unchecked_finish_with_return_block_is_not_safe :
    (uncheckedFinish outstandingReturnPreFinalState).phase = .closed ∧
    ¬ (uncheckedFinish outstandingReturnPreFinalState).resources.Quiescent := by
  constructor
  · rfl
  · simp [uncheckedFinish, outstandingReturnPreFinalState,
      Resources.Quiescent, Resources.HostDetached, Resources.CallsDrained,
      Resources.ReturnsDrained]

/-- The certified protocol therefore rejects finalization until all worksheet
returns and free callbacks have drained. -/
theorem outstanding_return_has_no_finish_certificate :
    ¬ Step outstandingReturnPreFinalState .finishClose
      (uncheckedFinish outstandingReturnPreFinalState) := by
  apply nonquiescent_cannot_finish
  simp [outstandingReturnPreFinalState, Resources.Quiescent,
    Resources.HostDetached, Resources.CallsDrained, Resources.ReturnsDrained]

/-- A fully reclaimed generation with two native Excel event roots. The roots
    are safe to invoke only because the resident module's ingress is closed. -/
def residentEventClosedResources : Resources :=
  { ingressOpen := false,
    residentEventRegistrations := 2,
    moduleResidencyHeld := true,
    callbackGateOpen := false,
    generationUnique := true,
    addinQuiesced := true,
    generationOwnedByRuntime := false }

theorem resident_events_allow_logical_close :
    residentEventClosedResources.Quiescent := by
  simp [residentEventClosedResources, Resources.Quiescent,
    Resources.HostDetached, Resources.CallsDrained, Resources.ReturnsDrained,
    Resources.AsyncDrained, Resources.SubscriptionsDrained, Resources.RtdDrained,
    Resources.HandlesDrained, Resources.GenerationReclaimed, Resources.DiagnosticsDrained]

theorem resident_events_do_not_allow_physical_unload :
    ¬ residentEventClosedResources.PhysicalUnloadSafe := by
  simp [Resources.PhysicalUnloadSafe, residentEventClosedResources]

theorem resident_events_without_lease_have_no_close_certificate :
    ¬ ({ residentEventClosedResources with moduleResidencyHeld := false } :
      Resources).Quiescent := by
  simp [Resources.Quiescent, Resources.HostDetached, residentEventClosedResources]

theorem resident_events_with_open_callback_gate_have_no_close_certificate :
    ¬ ({ residentEventClosedResources with callbackGateOpen := true } :
      Resources).Quiescent := by
  simp [Resources.Quiescent, Resources.HostDetached, residentEventClosedResources]

/-- The host can retain a terminal RTD server, class factory, and LockServer
    hold after backend/callback teardown. None authorizes physical unload. -/
def residentComClosedResources : Resources :=
  { residentEventClosedResources with
    residentEventRegistrations := 0,
    rtdClassFactories := 1,
    rtdServers := 1,
    rtdServerLocks := 1 }

theorem retained_com_roots_allow_logical_close :
    residentComClosedResources.Quiescent := by
  simp [residentComClosedResources, residentEventClosedResources, Resources.Quiescent,
    Resources.HostDetached, Resources.CallsDrained, Resources.ReturnsDrained,
    Resources.AsyncDrained, Resources.SubscriptionsDrained, Resources.RtdDrained,
    Resources.HandlesDrained, Resources.GenerationReclaimed, Resources.DiagnosticsDrained]

theorem retained_com_roots_do_not_allow_physical_unload :
    ¬ residentComClosedResources.PhysicalUnloadSafe := by
  simp [Resources.PhysicalUnloadSafe, residentComClosedResources]

theorem retained_com_roots_without_lease_have_no_close_certificate :
    ¬ ({ residentComClosedResources with moduleResidencyHeld := false } :
      Resources).Quiescent := by
  simp [Resources.Quiescent, Resources.RtdDrained, residentComClosedResources]

theorem retained_com_roots_do_not_excuse_live_operations :
    ¬ ({ residentComClosedResources with rtdOperations := 1 } : Resources).Quiescent := by
  simp [Resources.Quiescent, Resources.RtdDrained]

theorem retained_com_roots_do_not_excuse_live_callbacks :
    ¬ ({ residentComClosedResources with callbacks := 1 } : Resources).Quiescent := by
  simp [Resources.Quiescent, Resources.SubscriptionsDrained]

end XlFnFormal.Shutdown
