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

end XlFnFormal.Shutdown
