# xlfn shutdown formalization

This directory contains an executable Lean 4 specification of the XLL shutdown
protocol. It proves safety properties of the abstract protocol; it does **not**
by itself prove that the current Rust implementation refines the model.

## Toolchain

The project is pinned to Lean `v4.32.1`. It has no Mathlib or third-party
package dependency.

```text
cd formal
lake build
```

The CI job runs `leanchecker` (the official Lean 4 external kernel checker)
and rejects committed `sorry` or `admit` placeholders.

## Protocol represented by the model

The successful path is ordered as follows:

1. reject new work and detach Excel function/event registrations;
2. drain synchronous `CallGuard`s;
3. wait until Excel owns no DLL return block and no `xlAutoFree12` callback is
   executing;
4. cancel/drain async tasks and join the async executor;
5. terminate RTD operations, subscriptions, callbacks, class factories, COM
   servers, and server locks;
6. drain handle operations and stored handle values;
7. prove that there is no escaped state lease, worker, worker job, or other
   Add-in-owned resource, then consume the runtime's state root;
8. flush and join diagnostics;
9. enter `closed` only when every resource class is quiescent.

Any boundary or shutdown failure that prevents one of these postconditions
from being established must transition to `failStopped`. In an in-process XLL,
this denotes aborting or another host-level mechanism that prevents module
unload. It is intentionally not a recoverable `closed` state.

## Main theorems

`ExcelXllFormal/Shutdown/Invariant.lean` proves:

- `Step.certified_preserved`, the cumulative stage-invariant preservation theorem;
- `Reachable.certified` and `Steps.certified`, lifting it to arbitrary traces;
- `reachable_finalize_is_quiescent`, showing that the ordered milestones and
  resource-creation gates establish full quiescence before finalization.

`ExcelXllFormal/Shutdown/Safety.lean` proves:

- `Step.closed_target_is_quiescent`;
- `reachable_closed_is_quiescent`;
- `Steps.successful_shutdown_is_quiescent`;
- `reachable_closed_has_no_executable_work`, including return blocks and
  in-flight `xlAutoFree12` callbacks;
- `closed_terminal` and `failStopped_terminal`;
- `Step.phaseRank_mono` and `Steps.never_reopens`;
- `Step.externalAdmission_requires_open`;
- `stateEscape_cannot_reach_closed`;
- `nonquiescent_cannot_finish`;
- the bundled certificate `shutdownSafety`.

`ExcelXllFormal/Shutdown/Refinement.lean` proves the implementation bridge
`concrete_successful_shutdown_is_quiescent`.

`Counterexample.lean` demonstrates that an unchecked assignment of the
`closed` phase admits a closed state with an active call, and proves that this
operation has no `Step` certificate.

## Rust refinement obligations

The Rust implementation should expose one linearization point for each model
event. The principal mapping is:

| Lean event/stage | Rust responsibility |
|---|---|
| `beginClose` | `Runtime::begin_final_close` closes admission gates |
| `hostDetached` | all function and event registrations are removed |
| `callsDrained` | `Runtime::wait_for_calls` returns with count zero |
| `returnsDrained` | no live `ReturnBlock`; no `xlAutoFree12` is executing |
| `asyncDrained` | cancel tasks, await completion, join executor |
| `rtdDrained` | close subscriptions and wait for callbacks, factories, servers, locks, and COM operations |
| `handlesDrained` | close handle runtime and drop stored values |
| `stateClosed` | no escaped `Arc<State>`; `Addin::quiesce` joins workers before best-effort cleanup |
| `diagnosticsDrained` | flush and join diagnostic dispatcher |
| `finishClose` | call `Runtime::finish_close` only after `Quiescent` is checked |
| `failStop` | do not return to Excel when quiescence cannot be established |

`ShutdownRefinement` formalizes that integration boundary. The Rust runtime now
contains concrete counters and admission gates for DLL-owned return blocks,
in-flight `xlAutoFree12` callbacks, handle leases, COM class factories, COM
server objects, `LockServer` holds, and in-flight COM calls. Checked
subscription/handle teardown and terminal fail-stop propagation prevent a
cleanup failure from being converted into `closed`.

These counters are implementation evidence, not yet a machine-checked
refinement proof. The next integration step is to add a Rust-side ghost
event log, prove or test every `stepSound` obligation, and connect the checked
`Runtime::finish_close` path to `successIsClosed`. Property tests can then
compare implementation traces with this transition system.

See the repository [lifecycle guide](../guide/src/lifecycle.md),
[testing guide](../guide/src/testing.md), and [security model](../guide/src/security.md)
for the implementation-facing lifecycle, qualification, and deployment requirements.
