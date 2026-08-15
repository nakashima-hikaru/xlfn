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
an abstract structured identity and `RtdKey` is the corresponding abstract RTD
lookup key. `State` extends `Runtime.State` with the visible topic table, the
RTD reverse table, and initializer owners, so lifecycle phase and seal gates are
shared with the H2 model.

Pending allocation is separate from visible publication:

`beginInitializer → insertPendingFresh/Reuse → publishVisible(provisional) →
commitPublication → finishInitializer`.

`insertPendingFresh/Reuse` only updates the Runtime pending root. The
`.open`-only `publishVisible` step is the sole transition that appends to
`byKey`; `sealTopics` clears `byKey` and therefore models the concrete close
boundary where visible topics are discarded. Pending insertion remains
available in `drainingPrepares`, matching the seal-before-insert race.

Rollback resolves the Runtime pending root after the visible entry has either
been withdrawn or cleared by sealing. The current checker and safety layer
prove these single-flight/root obligations:

- a key has at most one active initializer owner;
- a key has at most one visible topic and therefore at most one committed topic;
- distinct visible topics have distinct registry tokens;
- every H3 initializer is backed by a Runtime initializer with the same id;
- a provisional topic is linked to the matching `Runtime.InitializerId` and
  `pending(token)` root;
- every committed topic has a live registry token.

The last property is expressed through `Registry.TokenLive`, so a committed
topic cannot retain an unpublished or stale registry root. `closeRegistry` and
`finishClose` are included in the model; reachable closed states carry a
`CloseCertified` certificate containing Runtime quiescence, empty visible and
reverse topic tables, and no H3 initializer owners.

## Handle topic reverse-map consistency: H3.2

H3.2 adds `RtdKey`, `Topic.rtdKey`, and `State.byRtdKey`. `publishVisible`
creates the `byKey` and `byRtdKey` entries in one transition; `withdrawVisible`
and `sealTopics` remove both sides together, and `closeRegistry` requires both
tables to be empty. The invariant separates the two directions of the
relationship:

- `ReverseMapSound`: every reverse entry names a visible topic with the same
  `TopicKey` and `RtdKey`;
- `ReverseMapComplete`: every visible topic has a matching reverse entry;
- `RtdKeysUnique`: distinct visible topics cannot share an RTD key;
- `ReverseRtdKeysUnique`: distinct reverse entries cannot share an RTD key.

The executable checker enforces the same reverse-key precondition and update
shape. The Rust publication boundary also rejects an existing `by_key` or
`by_rtd_key` entry with an internal fail-closed error instead of allowing a
`HashMap` overwrite. `reverse_lookup_resolves_visible_topic` exposes the
resolved RTD key and proves that a successful `topic_key_for_rtd`-style lookup
resolves to a visible topic, while `visible_topic_has_reverse_lookup` proves
that lookup returns an entry whose key is the topic identity. RTD string
formatting and byte-level serialization are kept as a separate boundary from
the H3.2 ownership invariant.

The H3.1/H3.2 replay fixtures are `fixtures/topics/success.json`,
`fixtures/topics/seal-before-visible-rollback.json`, and
`fixtures/topics/seal-after-visible-rollback.json`, plus
`fixtures/topics/observe-failure-rollback.json` (with `rollback.json` kept as
the short compatibility name). Publication fixtures now carry an explicit
`rtdKey`; the observe-failure path exercises paired `withdrawVisible` removal
before registry rollback. They use the same event vocabulary as
`XlFnFormal.Handle.Topics.Checker`, and Lean proves replay and close
certification for all four paths.

## Excel topic ownership and connection transactions: H3.3

H3.3 adds the structured `ExcelOwnerId` (`serverGeneration` plus `topicId`),
`Topic.excelOwner`,
`Topic.excelCommitted`, and the reverse ownership table
`State.byExcelOwner`. `beginConnection` claims a free visible topic and a
free owner atomically; `commitConnection` changes only the topic's Excel
commit flag; `reuseCommittedConnection` is an idempotent no-op for an already
committed owner/topic pair; and `rollbackConnection` removes only the
provisional Excel binding. A connection transaction is independent of the
formula publication transaction, so a formula-provisional topic may have a
committed Excel connection while it is still awaiting `commitPublication`.

The H3.3 invariant proves owner-map soundness and completeness, unique owner
bindings, binding uniqueness, and the implication that an Excel-committed
topic has an owner. `withdrawVisible` removes a paired Excel owner binding
when the topic is withdrawn, and `sealTopics` clears visible topics, reverse
RTD entries, and owner bindings at the same seal boundary. `closeRegistry`
requires all three tables and initializer owners to be empty.

Formula resolution and withdrawal also require
`Topic.ExcelConnectionSettled`: the topic must either have no Excel owner or
have an already committed Excel connection. This models the concrete
`observe`/`ConnectData` ordering, where an uncommitted connection is rolled
back before formula publication or withdrawal can continue. A committed Excel
connection may still coexist with a formula-provisional topic; the two
transactions remain independent after the Excel transaction settles.

Lean replay covers connection success, connection of an existing formula
topic, committed reuse, provisional
connection rollback while the formula topic remains visible, observe failure
after a committed Excel connection, seal-after-visible rollback, and reuse of
an owner on a second topic after rollback. The corresponding JSON fixtures
are `fixtures/topics/excel-connection-success.json`,
`fixtures/topics/excel-existing-topic-connection.json`,
`fixtures/topics/excel-connection-reuse.json`,
`fixtures/topics/excel-connection-rollback.json`,
`fixtures/topics/excel-observe-failure-rollback.json`,
`fixtures/topics/excel-seal-after-visible-rollback.json`, and
`fixtures/topics/excel-owner-reuse.json`. The negative fixture
`fixtures/topics/excel-unsettled-connection-rejected.json` records the
rejected `commitPublication` attempt while a connection is provisional. The
existing seal-before-visible
fixture covers the pending allocation race where no visible Excel owner can
be claimed. Invalid provisional reuse, unsettled formula resolution/withdrawal,
and owner collisions are rejected by the executable checker.

The H3.3 Safety surface exposes named theorems for owner lookup soundness and
completeness, committed-owner consistency, distinct-owner uniqueness,
rollback preservation of the formula topic and registry root, paired owner
removal on withdrawal, state-preserving committed reuse, and rejection of
formula resolution with an unsettled Excel connection.

## RTD server-generation isolation: H3.4

H3.4 makes the RTD server generation explicit in the topic model. A topic
stores an optional `serverGeneration`, and an Excel owner is the structured
pair `(serverGeneration, topicId)`. The independent `claimServer` event sets a
previously unclaimed topic generation and is idempotent for the same
generation; a different generation is rejected.

`beginConnection` applies the same generation compatibility rule directly,
matching the concrete `connect_inner` path rather than requiring a preceding
claim event. `commitConnection` and `reuseCommittedConnection` require an
owner whose generation matches the topic. `rollbackConnection` removes only
the provisional Excel binding and deliberately preserves `serverGeneration`,
so a stale server cannot reclaim the topic after rollback.

The reachable invariant includes
`ExcelOwnerGenerationConsistent`: every Excel owner has the same generation
as its topic, and committed connections retain that correspondence. Named
Safety theorems cover generation-mismatched claims and connections, owner and
topic generation agreement, committed-connection agreement, and generation
survival across rollback.

Executable replay covers claim plus connection success, same-generation claim
idempotence, mismatched claims, mismatched connection attempts, rollback
followed by a stale-generation attempt, same-generation reuse with another
topic id, committed reuse, and committed-topic rejection of a different
generation. The JSON fixtures are
`fixtures/topics/server-generation.json`,
`fixtures/topics/server-generation-mismatch-rejected.json`, and
`fixtures/topics/server-generation-rollback-rejected.json`.

Generation-wide topic termination is modeled in H3.5 by the detach-and-drain
transaction described below; H3.4 remains the isolation and ownership
provenance layer used by that transaction.

The allocator boundary is modeled independently under
`XlFnFormal/Rtd/ServerGeneration`. Its `State.last` is a non-wrapping natural
number, `maxGeneration` is `2^64 - 1`, and `allocate?` rejects an exhausted
state rather than producing zero or reusing a prior generation. The safety
layer proves strict increase, non-zero and bounded allocation, rejection at
the maximum, and non-reuse across sequential allocations. The Rust
`LAST_SERVER_GENERATION` counter uses a checked `fetch_update`; when the
counter is exhausted, `ensure_server` returns an internal error before
constructing or publishing a new RTD server.

## RTD wire serialization boundary

`XlFnFormal/Handle/Topics/Serialization` models the concrete RTD wire identity
without reusing the abstract H3 topic key. `FormulaTopicKeyWire` stores the
sheet id as a `Nat` with an explicit 32/64-bit `PointerWidth` bound, row and
column as bounded `Int` values corresponding to Rust `i32`, the UDF id as its
UTF-8 byte sequence, and the argument digest as exactly 32 bytes.

`formatRtdKey` emits the concrete byte layout: canonical decimal fields joined
by byte `0x1f`, followed by the UDF bytes and exactly 64 lower-case hex digest
bytes. The parser does not split every separator. It consumes the first three
separators for the numeric fields and locates the final separator plus the
fixed-size digest suffix, so a separator byte inside the UDF id is preserved.
The suffix separator is checked explicitly. `parseRtdKey` is the raw structural
parser, while `parseCanonicalRtdKey` additionally requires a formatter
round-trip, rejecting alternate decimal spellings such as leading zeroes.
`parseRtdKeyFor` is the executable, bounds-checked canonical parser.

The safety layer proves canonical decimal and digest parsing, UTF-8 validity
for encoded strings, wrong-separator rejection, `parse_format_roundtrip`,
`parseCanonical_format`, `parseCanonical_sound`, bounded parser soundness, a
`WellFormed ∧ re-encode` certificate, and `format_injective`. The checked
golden vectors are in `fixtures/topics/serialization-golden.json`; Lean proves
the same zero, i32-boundary, 64-bit Unicode, and embedded-separator cases in
`XlFnFormal.Handle.Topics.Serialization.Golden`, while Rust tests consume the
shared JSON fixture. The `serialization_golden_checker` executable consumes
that same fixture through the Lean parser and formatter in CI.

This serialization proof is intentionally downstream of H3.2: reverse-map
consistency does not depend on formatter injectivity, while the Rust producer
refinement can use these theorems to show that concrete RTD keys cannot
collide.

## H3.5 destruction boundary: detach and drain

The concrete ordering permits `DisconnectData` to overlap `ConnectData`. The
server operation gate admits both ordinary COM operations, and `ConnectData`
releases the topic lock after creating its provisional Excel connection but
before writing the value and committing that connection. `DisconnectData` can
therefore remove the visible topic and registry root while the connection is
still provisional; it does not inspect `excel_topic_committed`. The Rust
regression `disconnect_can_remove_pending_formula_root_during_excel_connection`
fixes this reachability.

The formal model now represents the lock boundary with `DetachedTopic` and a
separate `DestructionEvent` layer. `disconnectTopic` removes the `byKey`,
`byRtdKey`, and `byExcelOwner` entries but retains the registry token in the
detached list. The drain is deliberately split:

* `drainPendingReuse` / `drainPendingRetire` call the existing Runtime pending
  rollback transitions, resolve the initializer, and invalidate the token.
* `drainPublishedReuse` / `drainPublishedRetire` remove a committed root
  directly through the Registry transition, but require that no runtime
  initializer still owns the token as `.pending`.

`closeRegistry` and `CloseCertified` now also require `detached = []`, so a
detached root cannot be hidden by the close certificate. The named safety
surface proves that disconnect retains the root until drain, pending and
published drains invalidate it, and a published drain rejects a pending root.
`DestructionTrace` replays both the provisional-connection race and the
settled published-root path through `CloseCertified`; a negative replay proves
that the published drain cannot bypass pending rollback.

Generation-wide `terminate_topics(server_generation)` is modeled by
`detachGeneration`. It atomically removes all visible topics, reverse RTD
entries, and Excel-owner bindings belonging to the target server generation,
then moves their roots into the detached set. Registry-root destruction
reuses the existing pending/published drain transitions. The model proves
that nonmatching server generations retain their topics, reverse mappings,
Excel ownership, and live registry roots; provisional and published
generation-termination traces are replayed through `CloseCertified`.

## Published-topic snapshot refinement: H4.1

`XlFnFormal/Handle/Refinement` models the published-topic fast path as a
refinement layer over the canonical H3 topic state. A `Publication` records one
`(key, token)` incarnation and its lifecycle state (`provisional`, `live`,
`stale`, or `closing`); the snapshot map contains only the currently published
bindings, while a warm reader retains the identity it loaded before
observation. This keeps publication-object lifetime distinct from snapshot
membership and makes same-key replacement an explicit ABA boundary.

The relational H4 `Step` composes H3 `Topics.Step` and
`Topics.DestructionStep`: publication installation is combined with H3
`publishVisible`, activation is combined with H3 `commitPublication`, and a
cold observation failure withdraws the provisional publication through H3
`withdrawVisible`. Disconnect and generation termination update both layers,
and close delegates to H3 `closeRegistry`.
Warm readers and H3 initializer owners are accounted for together:
`warmReads.length + topics.runtime.initializers.length` must not exceed H3
`activePrepares`. Thus prepare entry/exit, initializer completion, warm-reader
drain, and close quiescence are part of the same reachable state. A failed
warm observation is represented by `failWarmRead`; it removes only the reader
and leaves a live publication intact, while the cold provisional failure path
uses the combined withdraw event.

The invariant proves publication identity uniqueness, provisional/live
canonical soundness, snapshot soundness and root liveness, warm-reader
provenance, and preservation for every H4 step. The checker has independent
sound and complete per-event validation against the relational step relation,
and replay traces are lifted to reachable invariant preservation. Named safety
theorems cover successful, failed, invalidated, and closing warm reads,
generation termination, same-key replacement, and registry close only after
both warm-reader and H3 prepare quiescence. Lean traces cover cold
publication, warm success/failure, disconnect, generation termination, close,
and a reachable same-key ABA sequence. Rust regressions cover a warm
observation racing generation-wide termination and a warm reader that must not
follow a recreated publication for the same key.

## H4.2 Rust-to-Lean handle trace bridge

The `handle-refinement-trace` feature adds a production-path recorder without
adding recorder state or event calls to the default build. The recorder emits
the H4 event schema at the same lifecycle boundaries used by the concrete
runtime: prepare entry/exit, initializer ownership, pending registry
allocation, provisional publication, publication activation, warm observation,
Excel connection transactions, disconnect, generation termination, and close.
Tokens, Excel owners, topic keys, and canonical RTD wire keys are serialized in
the trace so the Lean boundary checker can validate identity and serialization
correspondence rather than replaying labels alone.

Windows x86 and x64 test jobs write cold-success, cold-observation-failure,
warm-disconnect, warm-generation-termination, same-key ABA, and warm-close
traces as CI artifacts. The Lean `published_trace_checker` executable parses
those artifacts, checks the canonical RTD key encoding, and replays every event
through the relational H4 checker. Invalid close/quiescence fixtures are also
required to be rejected. This keeps the Rust producer and Lean replay in the
same CI boundary while leaving the H3 ownership and destruction invariants as
the source of truth.

## H4.3 PublishedHandle registry snapshot refinement

`XlFnFormal/Handle/Registry/Snapshot` formalizes the `PublishedHandle`
fast-lookup architecture as a refinement layer over the canonical H1 `Registry`
semantics:

* **Model & Invariants** (`Model.lean`): Tracks `publications`, active
  `snapshot` slot bindings, and active `fastLookups` (with two stages:
  `.tentative` and `.validated`) refining `registry`. Invariants preserve
  publication uniqueness, snapshot uniqueness, fast lookup uniqueness,
  fast lookup provenance, canonical live root correspondence, lease accounting
  (`validatedFastLookups.length ≤ activeLeases`), and closed registry emptiness.
* **Transitions & Soundness/Completeness** (`Transition.lean`, `Checker.lean`):
  Defines relational `Step` transitions:
  - `beginTentativeFastLookup`: Reader checks first `Live` state and acquires
    tentative implementation lease without touching H1 registry.
  - `validateFastLookup`: Reader checks second `Live` state, linearizing to H1
    `beginLookup` and advancing to `.validated`.
  - `rejectTentativeFastLookup`: If removed before second check, reader observes
    `Stale`/`Closing`, releases tentative lease, and exits without modifying H1.
  - `completeFastLookup` / `fallbackFastLookup`: Linearizes to H1 `endLookup`.
  - `beginSlowLookup`, `endSlowLookup`, `insertFresh`, `insertReuse`, `removeReuse`, `removeRetire`, `closeRegistry`, `finishClose`.
  Executable `apply?` is proven sound and complete against `Step`.
* **Invariant Preservation** (`Invariant.lean`): Proves `Step.invariant_preserved`
  and `Reachable.invariant_preserved` across all transitions.
* **Safety & Race Properties** (`Safety.lean`):
  - *Linearization Point*: Proves the second `Live` state check linearizes
    fast lookups to H1 `beginLookup`; if removed first, tentative lookup is rejected
    without modifying H1 state.
  - *Lease Accounting & Close Gating*: Proves both tentative and validated fast lookups
    prevent `finishClose` until all leases are released (`fastLookups = []`).
  - *Slot Reuse ABA Protection*: Proves `stale_lookup_cannot_follow_reused_generation`.
  - *Weak Upgrade Fallback*: Proves fallback drops lease and falls back cleanly
    without assuming unconditional fast success.
* **Executable Replay Traces** (`Trace.lean`): Proves replay traces for fast
  lookup success, real remove-before-second-check race linearization, slot reuse ABA
  protection, and reachable Weak upgrade fallback through `CloseCertified`.
* **Rust Concrete Quiescence & Deterministic Race Regressions** (`crates/xlfn-core/src/handle/`):
  - `HandleLeaseState::seal`: Atomic admission gate with in-flight reservation
    counter ensuring no new independent lookup lease can be admitted after close.
  - `published_handle_reused_slot_retains_stale_publication_arc`: Verifies
    retained old `Arc<PublishedHandle>` stays `Stale` with dead weak reference.
  - `published_handle_remove_during_lease_acquire_linearizes_as_stale`: Deterministic
    2-thread race using `after_acquire_hook` verifying `remove()` during tentative lease
    returns `StaleHandle` with 0 residual leases.
  - `published_handle_remove_before_weak_upgrade_falls_back_to_stale`: Deterministic
    2-thread race using `before_fast_upgrade_hook` verifying `remove()` after second Live
    check causes Weak upgrade failure, clean slow fallback, and 0 residual leases.

