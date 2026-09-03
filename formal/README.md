# xlfn formal verification

This directory contains Lean 4 specifications of the XLL lifecycle,
shutdown, handle registry, topic ownership, and published handle/topic
concurrency protocols.

## Scope

The formal model specifies and verifies the critical lifecycle and
concurrency protocols implemented in `xlfn`:

- **Lifecycle synchronization**: Opening, explicit removal, controlled reload, open-rollback, and terminal coordination.
- **Resource shutdown**: Deterministic staged teardown, quarantine, and certificate-based quiescence, including separate registry-sealed and pin-drained milestones.
- **Physical residency**: A separate module self-reference that remains held across logical `closed` and is releasable only after the quiescence certificate.
- **Composition**: Verified composition of lifecycle states and resource shutdown sessions.
- **Handle safety**:
  - Handle registry slot allocation, generation monotonicity, and ABA protection.
  - Handle prepare accounting and call-scoped borrow lifetime tracking.
  - Handle topic ownership, reverse-mapping consistency, and Excel connection transactions.
  - Published-topic snapshots with lock-free warm reads and generation isolation.
  - Published-handle temporal ownership: Box-owned binding records, admission gates, and ObjectArena capability accounting.
  - Generic temporal ownership: DrainGate, AtomicPtr, and Box reclamation algebra with GenerationServiceSlot refinement.
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

open / opening / openRollbackPending ──requestExplicitRemoval──> closing
                                                               │
                                                        finishFinalRemoval
                                                               ↓
                                                             closed

Any live phase ──teardown hazard──> quarantined
```

An ordinary `xlAutoClose` is not represented as a terminal transition: it is a
host hint and leaves an open generation open. `xlAutoRemove` requests the
terminal transition. A second `xlAutoOpen` while open performs controlled
reload by completing the old removal and beginning a new generation.

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
uncommitted final removal and an open rollback carry only a resource-quiescence
witness because no Shutdown ghost generation exists on those paths.

### Shutdown

The successful explicit-removal path has one order in both the model and the
Rust teardown path:

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
- call-scoped handle borrows, published handles, and long-lived registry pins;
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

Handle shutdown has three distinct obligations. `handles = 0` means that no
formula binding roots remain; `handlePins = 0` means that no `HandleLease`
can still retain a retired payload; and `handleObjects = 0` means that no
published snapshot or pending ownership edge can still retain an
`ObjectCell`. The model therefore permits
the registry to be sealed before lease/object drain, but admits `handlesDrained` only
after all three counters are zero. This matches the Rust order: `FormulaHandleService::seal`
retires bindings, Add-in cleanup drops generation state, and only then can
`HandleStoreQuiescent` be issued.

The Rust shutdown refinement trace carries the same `handlePins` and
`handleObjects` counters and their corresponding add/remove events, so the
executable trace checker observes the same seal-versus-lease/object-drain
boundary as the Lean transition system.

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
  logicalQuiescenceCertified : Bool
```

The option is a current-session marker, not a test of historical
`generation ≠ 0`: a failed attempt after a previous committed generation
leaves the historical generation unchanged while the current marker remains
`none`. A committed open stores the concrete resource snapshot supplied by
the transition; it does not use a fixed empty resource value. The
`logicalQuiescenceCertified` is cleared when opening begins and is set only by
the successful explicit-removal publication paths; it remains available after
a committed session is retired. It is a logical quiescence certificate, not a
claim that the DLL's physical residency lease has already been released.

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
the logical-quiescence ghost invariant. `Composition/Safety.lean` proves the
quiescence result for all three successful removal paths, that a `ReturnSafe`
state cannot retain an active Shutdown session, and that a successful return
is logically quiescence-certified. `Composition/Checker.lean` provides the executable
`apply?` together with soundness and completeness against the relational
`Step` model.

`Composition/Refinement.lean` lifts those results across a concrete state
machine, establishing `concrete_successful_xlAutoRemove_is_safe`.

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

### Generic temporal ownership

`XlFnFormal/TemporalOwnership` formalizes the reusable `DrainGate + AtomicPtr + Box`
ownership algebra:

- **Concept**: A unique owner publishes an addressable pointer guarded by an admission gate.
  Readers acquire counted drain permits before loading.
- **Safety properties**:
  - `readerImpliesOwned`: Active readers guarantee the unique owner is alive.
  - `reclaimedImpliesNoReaders`: Reclaiming the owner is impossible while readers are active.
  - `sealedImpliesNoNewReaders`: A sealed gate rejects admission of new readers.
  - `reclaimRequiresUnpublishedAndDrained`: Owner reclamation requires an unpublished pointer and a fully drained gate.
  - `noUseAfterReclaim`: Reclaimed resources cannot be accessed by readers.
- **Service-slot refinement**: `GenerationServiceSlotRefinement.lean` provides a thin refinement
  mapping `GenerationServiceSlot` states to this generic algebra, verifying that `Ready` corresponds
  to an open gate with an owner, `Sealing` seals the gate, and `ServiceSeal` transfers `Box` ownership
  only after readers have drained to zero.

### Published handles and ObjectArena capabilities

`XlFnFormal/Handle/Publication` formalizes the published binding and object capability
architecture under the redesign:

- **Ownership model**: `BindingTable` owns `Box<BindingRecord>`. `PublishedBindings` exposes an
  `AtomicPtr<BindingRecord>`, and readers hold an `OwnedOperationGuard` (admission permit).
  `ObjectArena` is the sole owner of each `ObjectCell`. A published `BindingRecord` holds a non-owning
  `ObjectBinding` capability, while `HandleLease<T>` holds a counted `pin` capability.
- **Retirement protocol**:
  - `beginRetire`: Clears publication (`published = false`) and seals the admission gate.
  - `retireCapability`: Waits for reader drain (`admitted = 0`) before retiring the object capability
    and reclaiming the record.
  - `reclaimObject`: An `ObjectCell` is reclaimed only when both capability counts reach zero
    (`bindings = 0 ∧ pins = 0`).
- **Safety theorems**:
  - `readerImpliesOwned`: Call-scoped borrows imply the backing `BindingRecord` owner is alive.
  - `borrowedObjectNotReclaimed`: An object with active readers cannot be destroyed.
  - `pinnedObjectNotReclaimed`: An object held by a `HandleLease` pin cannot be destroyed.
  - `reclaimRequiresNoCapabilities`: `ObjectCell` destruction requires all capabilities to be drained.
  - Refinement to `TemporalOwnership`: Publication events simulate `TemporalOwnership.Step`.


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

Validates ordered lifecycle certificates and order-independent activity
ownership observations against the independent shutdown specification.

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

The formalization ends at the temporal ownership and capability boundaries.
Low-level NonNull address values, allocator implementation, atomic memory
ordering/interleavings, scheduler fairness, Excel internals, COM implementation
correctness, and arbitrary user code are outside the Lean model.

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
