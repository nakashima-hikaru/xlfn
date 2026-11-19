import XlFnFormal.Shutdown.Checker
import Lean.Data.Json

set_option autoImplicit false

namespace XlFnFormal.Shutdown

/-- Passive counts from the activity interval ending at a native diagnostic
    worker stop. The initial pending count is kept in the resource snapshot. -/
structure DiagnosticActivity where
  enqueued : Nat := 0
  flushed : Nat := 0
  discarded : Nat := 0
  deriving DecidableEq, Repr

namespace DiagnosticActivity

def parse (json : Lean.Json) : Except String DiagnosticActivity := do
  let activity : DiagnosticActivity := {
    enqueued := (← json.getObjValAs? Nat "enqueued")
    flushed := (← json.getObjValAs? Nat "flushed")
    discarded := (← json.getObjValAs? Nat "discarded")
  }
  -- A compact wire count must not expand beyond the native activity budget.
  if activity.enqueued + activity.flushed + activity.discarded > 16384 then
    throw "diagnostic stop projection exceeded its in-memory event budget"
  return activity

def add (left right : DiagnosticActivity) : DiagnosticActivity := {
  enqueued := left.enqueued + right.enqueued
  flushed := left.flushed + right.flushed
  discarded := left.discarded + right.discarded
}

/-- A canonical accounting order, not the order of concurrent native appends.
    Every expanded event is checked by the existing transition relation; in
    particular, stopDiagnostics still requires zero outstanding diagnostics. -/
def events (activity : DiagnosticActivity) : Array Event :=
  Array.replicate activity.enqueued .enqueueDiagnostic ++
  Array.replicate activity.flushed .flushDiagnostic ++
  Array.replicate activity.discarded .discardDiagnostic ++ #[.stopDiagnostics]

theorem accepted_stop_has_no_pending
    {source target : State}
    (accepted : apply? source .stopDiagnostics = some target) :
    source.resources.diagnosticsPending = 0 := by
  have step := apply?_sound accepted
  cases step with
  | stopDiagnostics _ empty _ => exact empty

-- Projection preserves startup debt: only an observed completion can spend it.
example : ({ flushed := 1 } : DiagnosticActivity).events.foldlM apply?
    (State.opened { diagnosticsPending := 1, diagnosticsRunning := true }) =
    some (State.opened { diagnosticsRunning := false }) := by
  rfl

example : ({} : DiagnosticActivity).events.foldlM apply?
    (State.opened { diagnosticsPending := 1, diagnosticsRunning := true }) = none := by
  rfl

example : ({ enqueued := 1, flushed := 1 } : DiagnosticActivity).events.foldlM apply?
    (State.opened { diagnosticsPending := 1, diagnosticsRunning := true }) = none := by
  rfl

example : ({ flushed := 2 } : DiagnosticActivity).events.foldlM apply?
    (State.opened { diagnosticsPending := 1, diagnosticsRunning := true }) = none := by
  rfl

end DiagnosticActivity

end XlFnFormal.Shutdown
