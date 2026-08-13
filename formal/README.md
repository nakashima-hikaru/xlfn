# xlfn formal verification

This directory contains Lean 4 specifications of the XLL lifecycle and
shutdown protocols. The models separate lifecycle synchronization from
resource shutdown and then provide a composition layer for the two paths.

## Toolchain

The project is pinned to Lean `v4.32.1` and has no third-party Lean package
dependency.

```text
cd formal
lake build
```

## Shutdown protocol

The successful path has one order in both the model and the Rust close path:

```text
open
  ↓ beginClose
drainCalls
  ↓ callsDrained
drainReturns
  ↓ returnsDrained
drainAsync
  ↓ asyncDrained
stopSubscriptions
  ↓ subscriptionsDrained
detachHost
  ↓ hostDetached
closeState
  ↓ stateClosed
drainHandles
  ↓ handlesDrained
stopDiagnostics
  ↓ diagnosticsDrained
drainRtd
  ↓ rtdDrained
finalize
  ↓ finishClose
closed
```

`beginClose` closes the unified external ingress. `callsDrained` therefore
follows both `ExportIngress::seal_and_drain` and
`Runtime::wait_for_calls`; `returnsDrained` follows
`Runtime::wait_for_returns`. Subscription teardown is separate from host
registration detachment, and RTD module quiescence is separate from both.

`Resources` contains only evidence available to the framework or supplied by
an explicit Add-in contract:

- ingress, external entries, calls, worksheet return blocks and free callbacks;
- async tasks and executor state;
- subscriptions, callbacks, RTD operations, factories, servers and locks;
- handle operations and published handles;
- registration state and callback-gate state;
- `stateUnique`, `addinQuiesced`, and `stateOwnedByRuntime` for Add-in state;
- diagnostics and cleanup-issue accounting.

Arbitrary user threads and native callbacks are not represented by unverifiable
ghost counters. `Arc::try_unwrap(state)` establishes `stateUnique`,
`Addin::quiesce` establishes `addinQuiesced`, and consuming the runtime root
establishes `stateOwnedByRuntime = false`.

`RtdDrained` is intentionally limited to RTD operations, class factories,
servers, and server locks. `SubscriptionsDrained` owns the separate
subscription/callback postcondition.

## Proofs

`XlFnFormal/Shutdown/Invariant.lean` proves cumulative certificate
preservation across the ordered stages. `Safety.lean` proves monotone phase
progress, terminal `closed`/`failStopped` states, external-admission gating,
and the `successIsClosed` obligations used by the refinement structure.
`Counterexample.lean` demonstrates why assigning `closed` without a quiescence
certificate is not a valid transition.

The Rust `shutdown_refinement` module is enabled only for tests or with the
`shutdown-refinement` feature. Its `GhostMachine::apply` method is the single
runtime transition implementation used for invariant checking; it rejects an
event before updating state. The optional `shutdown-trace` feature enables a
bounded event-only trace for the executable checker. Each successful open
starts a new generation, and each trace is tied to that generation. The JSON
trace includes `schema_version`, `generation`, `initial`, `events`, a
`trace_truncated` budget marker, and an explicit outcome.
The executable checker can validate a trace directly:

```text
lake exe shutdown_trace_checker < shutdown-trace.json
```

Recoverable diagnostic-worker replacement and reopenable drain failures remain
live traces: they record a cleanup issue, and any queue entries lost with the
panicked worker are recorded as discarded diagnostics. A terminal diagnostic
failure instead ends the generation with `failStop`.

The Lean model proves that an accepted abstract trace is safe. Rust tests are
still responsible for placing each event at the actual Rust linearization
point; the formal model does not inspect machine code or infer arbitrary
user-thread behavior.

See the repository [lifecycle guide](../guide/src/lifecycle.md),
[testing guide](../guide/src/testing.md), and
[security model](../guide/src/security.md) for implementation-facing
lifecycle and deployment requirements.

## Lifecycle synchronization protocol

`XlFnFormal/Lifecycle` formalizes the concurrency protocol around opening and
final closing independently of resource shutdown. The model tracks the phase,
close epoch, open attempt, cleanup owner, and committed generation:

```text
closed ──beginOpen──> opening ──finishOpen──> open
   ↑                       │                    │
   │                       └─failOpen──> openRollbackPending
   │                                            │
   └────────────finishOpenRollback─────────────┘

open / opening / openRollbackPending ──requestFinalClose──> closing
                                                               │
                                                        finishFinalClose
                                                               ↓
                                                             closed
```

`requestFinalClose` on `closed` only advances the close epoch; it does not
acquire a cleanup owner or perform another close.

`finishOpenRejectedByClose` and the closing form of `failOpen` clear the
uncommitted open attempt without publishing a generation. A final-close or
open-rollback owner may publish `closed` while its guard is still active;
`releaseCleanupOwner` is the separate return-safety point. Consequently
`State.ReturnSafe` requires `phase = closed`, no open attempt, and no cleanup
owner. `State.CanBeginOpen` also requires an exact close-epoch match.

`Invariant.lean` proves `WellFormed` preservation, `Safety.lean` captures the
lifecycle race properties, and `Checker.lean` provides an executable
`apply?` with soundness and completeness theorems. `Counterexample.lean`
records the three unsafe relaxations: ignoring the epoch, committing open
while closing, and reopening while a published-closed cleanup owner is still
active.

`Model.lean` also defines `State.Valid`, which strengthens `WellFormed` with
non-zero open-attempt and committed-generation identifiers, and
`State.initialState`, the logical counterpart of `Runtime::new()`. The logical
`AttemptId` and `Epoch` counters are non-wrapping `Nat`s. A Rust refinement
must establish that its `u64` counters do not wrap, or convert overflow into
fail-stop before emitting an abstract event.

The model is a safety transition system. It does not model condition-variable
wakeups, scheduler fairness, or eventual progress. In particular, the Rust
`close_waiter_is_not_lost_when_open_rollback_finishes` concurrency test remains
the obligation that checks waiter blocking, notification, and eventual return.

`Lifecycle/Certificate.lean` records the three cleanup certificate shapes. A
committed generation requires a quiescent Shutdown state at `finalize`; an
uncommitted final close and an open rollback carry only a resource-quiescence
witness because no Shutdown ghost generation exists on those paths.

`Composition/Model.lean` introduces:

```lean
structure ShutdownSession where
  generation : Lifecycle.AttemptId
  state : Shutdown.State

structure State where
  lifecycle : Lifecycle.State
  currentShutdown : Option ShutdownSession
  unloadCertified : Bool
```

The option is a current-session marker, not a test of historical
`generation ≠ 0`: a failed attempt after a previous committed generation
leaves the historical generation unchanged while the current marker remains
`none`. A committed open stores the concrete resource snapshot supplied by
the transition; it does not use a fixed empty resource value. The
`unloadCertified` ghost fact is cleared when opening begins and is set only by
the successful close publication paths; it remains available after a
committed session is retired.

`Composition/Transition.lean` keeps the Rust linearization points separate:
`commitOpen` creates a generation, `finishCommittedShutdown` moves the
Shutdown session to `closed`, `publishCommittedClosed` publishes the
lifecycle state, `retireCommittedShutdown` models the returned-success
record, and `releaseCleanupOwner` is the final return-safety point. An
abandoned close owner may release only its ownership while the lifecycle is
still `closing`, leaving the Shutdown session available for a takeover; the
`closed + some Closed` state still requires retirement before that owner can
be released. The uncommitted final-close and open-rollback transitions remain
session-free.

`Composition/Invariant.lean` proves `Valid` preservation and reachable-trace
validity, including lifecycle/Shutdown generation equality and preservation of
the unload-certification ghost invariant. `Composition/Safety.lean` proves
the quiescence result for all three successful close paths, that a `ReturnSafe`
state cannot retain an active Shutdown session, and that a successful return
is unload-certified. `Composition/Checker.lean` provides the executable
`apply?` together with soundness and completeness against the relational
`Step` model.

`Composition/Refinement.lean` lifts those results across a concrete state
machine. Its `CompositionRefinement` records the abstraction function,
linearization-point event relation, an explicit no-counter-wrap obligation,
and the obligation that a concrete successful return reaches `ReturnSafe`;
`concrete_successful_xlAutoClose_is_safe` then supplies the end-to-end theorem
for a concrete trace. A Rust adapter must discharge the counter obligation
before mapping wrapping `u64` operations into the logical `Nat` transitions.

`Composition/TraceChecker.lean` replays an event-only composition trace. The
trace schema is intentionally smaller than the shutdown trace because every
intermediate full state is reconstructed by `Composition.apply?`:

```json
{
  "schema_version": 1,
  "initial": "initial",
  "events": [],
  "trace_truncated": false,
  "outcome": "returned_success"
}
```

Parameterized events carry their concrete attempt/resource payloads, and
`liftShutdown` nests an existing Shutdown event. The six replay fixtures in
`fixtures/composition/` cover committed close, uncommitted final close, open
rollback, committed-owner takeover, an already-closed successful return, and
two committed close/open cycles in one runtime lifetime. They can be checked
with:

```text
lake exe composition_trace_checker < fixtures/composition/committed.json
```

The generic refinement boundary is now in place. The Rust feature-gated
`composition_refinement` producer emits composition events at the lifecycle
linearization points and converts the concrete quiescence certificates into
certificate-derived abstract resource images. A trace spans the full
`Runtime` lifetime, so close/open cycles retain the prior `closeEpoch` history;
an already-closed `xlAutoClose` return is recorded at its actual boundary.
The CI Lean job feeds those generated traces to `composition_trace_checker`,
including the committed-owner takeover, already-closed, and reopen paths. The
producer uses checked attempt allocation and fail-stop close-epoch advancing,
so the concrete side does not rely on wrapping `u64` counters.

## Handle topic ownership: H3.1

`XlFnFormal/Handle/Topics` is the first topic-ownership layer. `TopicKey` is
an abstract structured identity; RTD serialization and the reverse map are
deliberately out of scope until H3.2. `State` extends `Runtime.State` with the
visible topic table and initializer owners, so lifecycle phase and seal gates
are shared with the H2 model.

Publication is split into a provisional visible phase and a final commit:

`beginInitializer → insertPending → publishVisible(provisional) →
commitPublication → finishInitializer`.

Rollback removes the provisional topic before the Runtime pending root is
resolved. The current checker and safety layer prove these single-flight/root
obligations:

- a key has at most one active initializer owner;
- a key has at most one visible topic and therefore at most one committed topic;
- distinct visible topics have distinct registry tokens;
- a provisional topic is linked to the matching `Runtime.InitializerId` and
  `pending(token)` root;
- every committed topic has a live registry token.

The last property is expressed through `Registry.TokenLive`, so a committed
topic cannot retain an unpublished or stale registry root. H3.2 will add
`byKey`/`byRtdKey` consistency and RTD-key uniqueness before Excel ownership
and server-generation transactions are introduced.

The first H3.1 replay fixtures are `fixtures/topics/success.json` and
`fixtures/topics/rollback.json`. They use the same event vocabulary as
`XlFnFormal.Handle.Topics.Checker`; the producer-facing JSON parser will be
added when the Rust topic recorder is introduced.
