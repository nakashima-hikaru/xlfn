# xlfn formal verification

This directory contains Lean 4 specifications of the XLL lifecycle,
shutdown, handle registry, topic ownership, and published handle/topic
concurrency protocols.

## Scope

The formal model specifies and verifies the critical lifecycle and
concurrency protocols implemented in `xlfn-core`:

- **Lifecycle synchronization**: Opening, closing, open-rollback, and final-close coordination.
- **Resource shutdown**: Deterministic staged teardown and certificate-based quiescence.
- **Composition**: Verified composition of lifecycle states and resource shutdown sessions.
- **Handle safety**:
  - Handle registry slot allocation, generation monotonicity, and ABA protection.
  - Handle prepare accounting and call-scoped borrow lifetime tracking.
  - Handle topic ownership, reverse-mapping consistency, and Excel connection transactions.
  - Published-topic snapshots with lock-free warm reads and generation isolation.
  - Published-handle snapshots with strong RCU roots and call-scoped borrows.
  - RTD server-generation isolation and atomic detach-and-drain transactions.
  - RTD wire serialization and parser injectivity.

---

## Lifecycle and shutdown

### Lifecycle

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
`AttemptId` and `Epoch` counters are non-wrapping `Nat`s.

`Lifecycle/Certificate.lean` records the three cleanup certificate shapes. A
committed generation requires a quiescent Shutdown state at `finalize`; an
uncommitted final close and an open rollback carry only a resource-quiescence
witness because no Shutdown ghost generation exists on those paths.

### Shutdown

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
  ↓ generationReclaimed
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
- call-scoped handle borrows and published handles;
- registration state and callback-gate state;
- `generationUnique`, `addinQuiesced`, and `generationOwnedByRuntime` for open generation reclamation;
- diagnostics and cleanup-issue accounting.

Arbitrary user threads and native callbacks are not represented by unverifiable
ghost counters. `Arc::try_unwrap(generation)` establishes `generationUnique`,
`Addin::quiesce` establishes `addinQuiesced`, and consuming the runtime root
establishes `generationOwnedByRuntime = false`.

`RtdDrained` is intentionally limited to RTD operations, class factories,
servers, and server locks. `SubscriptionsDrained` owns the separate
subscription/callback postcondition.

`XlFnFormal/Shutdown/Invariant.lean` proves cumulative certificate
preservation across the ordered stages. `Safety.lean` proves monotone phase
progress, terminal `closed`/`failStopped` states, external-admission gating,
and the `successIsClosed` obligations used by the refinement structure.
`Counterexample.lean` demonstrates why assigning `closed` without a quiescence
certificate is not a valid transition.

### Composition

`Composition/Model.lean` composes lifecycle synchronization and resource shutdown:

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
machine, establishing `concrete_successful_xlAutoClose_is_safe`.

---

## Handle safety

### Registry

`XlFnFormal/Handle/Registry` formalizes the core handle table semantics:
slot allocation, 64-bit generation monotonicity, live count accounting,
and slot-reuse ABA protection. It proves that obsolete tokens cannot resolve
to newly inserted entries and that registry close drains all held roots.

### Runtime

`XlFnFormal/Handle/Runtime` models prepare synchronization and the registry's
call-scoped borrow accounting during lookup. The public Rust `Handle<'call, T>`
cannot outlive its Excel call; the formal borrow counter records that call
boundary while the registry closes only after those borrows have returned.

### Topic ownership

`XlFnFormal/Handle/Topics` models topic registration, visibility transitions,
and ownership guarantees:

- Pending allocation is separate from visible publication:
  `beginInitializer → insertPendingFresh/Reuse → publishVisible(provisional) → commitPublication → finishInitializer`.
- Each key has at most one active initializer and at most one visible topic.
- Visible topics have distinct registry tokens and are backed by live registry entries.
- `State.byRtdKey` maintains strict bidirectional consistency:
  `ReverseMapSound`, `ReverseMapComplete`, `RtdKeysUnique`, and `ReverseRtdKeysUnique`.
- Excel connection transactions (`beginConnection`, `commitConnection`, `rollbackConnection`, `reuseCommittedConnection`)
  track Excel owners `(serverGeneration, topicId)` and isolate provisional bindings from committed ones.
- Detach-and-drain semantics (`disconnectTopic`, `detachGeneration`, `drainPending*`, `drainPublished*`)
  safely resolve overlapping `ConnectData` and `DisconnectData` operations without leaking roots.

### Published-topic snapshots

`XlFnFormal/Handle/Refinement` models the published-topic fast path as a
refinement layer over the canonical topic state:

- Tracks `Publication` objects with explicit lifecycle states (`provisional`, `live`, `stale`, `closing`).
- Warm readers observe immutable snapshot bindings without locking the canonical topic map.
- Linearization points: publication installation is combined with `publishVisible`, activation with `commitPublication`,
  and observation failure with `withdrawVisible`.
- Disconnect and generation termination update both layers, while close delegates to `closeRegistry`.

### Published-handle snapshots

`XlFnFormal/Handle/Registry/Snapshot` formalizes the `BindingRecord`
fast-lookup architecture as an RCU layer over canonical registry semantics:

- **Model & Invariants** (`Model.lean`): Tracks published objects, the current
  immutable snapshot, and active call-scoped `Borrow` records. A publication
  remains rooted after it becomes `stale` or `closing` until no borrow refers to
  it, matching the strong `Arc<BindingRecord> → Arc<HandleObject>` chain.
- **Transitions & Checker** (`Transition.lean`, `Checker.lean`):
  `observeBorrow` performs the snapshot lookup, generation/authentication and
  `Live` check at the borrow linearization point. `releaseBorrow` ends the call
  scope. Removal unpublishes the slot but does not destroy a borrowed object;
  `retirePublication` is allowed only after both the snapshot and all borrows
  have released it. Close clears the new-reader snapshot and marks remaining
  publications closing without a second admission protocol.
  The executable checker accepts exactly these transitions, and the safety
  lemmas cover stale/closing rejection, borrow-root preservation, and
  reclamation only after the last borrow returns.

### RTD server generations

`XlFnFormal/Rtd/ServerGeneration` models the RTD server generation allocator.
Generations are non-wrapping, non-zero natural numbers bounded by `2^64 - 1`.
The safety proofs establish strict monotonicity, exhaustion detection, and
non-reuse across sequential allocations.

### RTD-key serialization

`XlFnFormal/Handle/Topics/Serialization` models the concrete RTD wire identity:

- Formats `(sheetId, row, col, udfId, inputFingerprint)` into canonical decimal/hex fields separated by `0x1f`.
- Proves UTF-8 validity preservation, `parse_format_roundtrip`, `parseCanonical_sound`, and `format_injective`.
- Golden vectors (`fixtures/topics/serialization-golden.json`) are shared directly between Rust and Lean CI checks.

---

## Executable validation

Executable Lean checkers validate trace fixtures and Rust-produced execution traces:

### Shutdown trace checker

```text
lake exe shutdown_trace_checker < shutdown-trace.json
```

Validates staged shutdown event ordering and resource quiescence certificates.

### Composition trace checker

```text
lake exe composition_trace_checker < composition-trace.json
```

Replays lifecycle and shutdown composition traces across open/close generations.

### Published-topic trace checker

```text
lake exe published_trace_checker < published-trace.json
```

Validates concrete Rust-generated traces of topic publication, warm observation,
Excel connection transactions, and generation termination against the formal model.

### Serialization golden checker

```text
lake exe serialization_golden_checker < fixtures/topics/serialization-golden.json
```

Validates RTD key parser and formatter against canonical golden vectors.

---

## Verification boundary

The formal model covers the framework-owned concurrency and lifecycle
protocols whose violations can produce stale handles, escaped resources,
or unsafe shutdown.

The formalization ends at the BindingRecord publication protocol. Rust memory safety,
ArcSwap internals, atomic implementation semantics, scheduler fairness,
Excel internals, COM implementation correctness, and arbitrary user code
are outside the Lean model.

Correspondence with the Rust implementation is checked by targeted
concurrency tests and the retained feature-gated trace checkers; it is not
a machine-code refinement proof. Refinement traces are ephemeral build/test
artifacts generated in-tree and validated by the checker built from the same source
revision. No backward compatibility contract is provided for refinement trace JSON.

---

## Building

The project is pinned to Lean `v4.32.1` and has no third-party Lean package
dependencies.

```text
cd formal
lake build
```
