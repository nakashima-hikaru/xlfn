# Production refinement completion audit

The objective is the main unsafe ownership path, not a count of passing model
proofs. A row closes only when production-shared logic, its proof composition,
and the relevant validation are present. Synchronization/library semantics may
remain in the TCB; their correct use and ownership preconditions may not be
silently assumed as the desired conclusion.

| Requirement | Authoritative production surface | Evidence required | Status |
| --- | --- | --- | --- |
| Counter admission/release, widths, fail-stop | `sealable_counter/transitions.rs` | Shared 32/64-bit executable transition proofs | Implemented |
| Final release and wait observation | `drain_gate.rs`, `drain_gate/protocol.rs` | Shared lock/release/notify/unlock and reobservation proofs, backend obligations | Shared control flow implemented; backend composition remains |
| Striped drain/idle seal/rollback | `drain_gate.rs` | All-stripe conservation and zero observations, rollback preserves waiting | Shared full-stripe scan, linear instance ledgers and sealed-zero interference histories composed; idle-seal/rollback verified; native address/history/lock representation remains open |
| Reader selection/retry and generation identity | `rotating_read_domain.rs` | Admission at the gate RMW, including stale selection and reuse | Production borrowed/owned and Loom reader loops share the verified selection/acquire/retry/permit control flow; 32/64-bit acquire rejects noncurrent selection under the generation invariant; live history, stripe and allocation identity composition open |
| Rotation and pending generation | `rotating_read_domain.rs` | Shared blocking/polled/idle paths; seal before publish before reopen; pending retained on callback failure | Shared begin/publish/reopen/finish and polled observation order verified in 32/64-bit models; full driver/idle-seal composition open |
| Publication barrier and terminal close | `rotating_read_domain.rs` | Registration held through publication, released before wait; close prevents reopen; no fabricated drain | Shared barrier/publication order and close-before-seal verified; actual SeqCst current and queue-handle registration derive readiness by token custody, with reserve/publish resource transfer; native driver and both-generation drain composition open |
| Cache observation/pin/retirement/reclaim | `cache.rs`, `cache/pin_transitions.rs` | Shared ownership transitions connected to the generation proof; pin/read/reclaim exclusion | Shared pin kernel; heap-backed pin/observation/retirement resources; scoped rotation/stripe exclusion and shared typed queue registration/detachment composed; frozen observation coverage narrows before shared publication and pending drain derives zero observations; native queue/stripe cutoff and representation remain open |
| Handle observation/retirement/debt/reclaim | `handle/domain.rs`, `handle/binding.rs`, `handle/object.rs` | Same-domain admission, queue-generation revalidation, linear retirement, pin/binding lifetime | Production registration, certificate-authorized detachment and completion tail share control flow; modeled debt includes destruction in flight and linear return receipts; shared checked debt/queued arithmetic and conditional count bridges added; native owner-borrowing drained batches added with shared merge identity checks; initialized linear binding owners now traverse shared registration/certified batches/completion in Verus; native reader witnesses borrow their retaining scope and shared append-only storage conserves linear admissions; enriched AtomicPtr retirement records now traverse registration, selected-queue coverage preparation, certified detachment and exact batch heap recovery; a library queue lock now preserves payload invariants and composes acquisition, preparation, publication and release; native lock/array/address/history and Box/Drop adapters plus concurrent atomic debt composition remain open |
| Allocation ownership/raw pointer recovery | `published_owner.rs` and callers | Single allocation ownership, borrow/access capability, recovery after all relevant readers/pins | Tracked initialized memory plus allocator rights retained through owner/cache retirement proofs; native Box/DST/Drop and reader-lifetime adapters remain open |
| Cross-layer composition and claim audit | Lean invariants, Verus, unsafe call sites | Requirement-to-proof/caller mapping; explicit remaining TCB, no theorem claims for models alone | Open |
| Regression sensitivity and validation | `Justfile`, verification tools, kernel/cache/handle tests | Unsafe shared mutations rejected; Verus/audit, Loom, both Miri modes, Rust checks | Shared-control and resource negative gates implemented; incremental native/Loom/Miri validation recorded below; final whole-stack audit remains open |

Policy: do not add project-owned trusted adapters; unsupported primitive
connections remain explicitly incomplete.

The current native field/lock mismatches and next representation change are
listed in [NATIVE_ADAPTER_PLAN.md](NATIVE_ADAPTER_PLAN.md). In particular, the
Cache proof currently allocates a separate pin atomic and retains an executable
ledger RwLock for observation registration/completion and retirement. Pin CAS and
metadata reads now borrow the reader-owned Ticket without that lock; the remaining
representation and synchronization differences are not native refinement. Primitive contracts alone would not remove that difference.

Windows execution and remote CI evidence are distinct from local Rust/Verus
checks. This worklist is not a release-readiness or publication authorization.

## Current evidence

- RotatingReadDomain's abstract model now separates publish from reopen, rejects
  stale selected gates by the sealing invariant, and separates closure start,
  sealing and completed idle observations. Closure does not erase reader counts.
- Its production-shared macro bodies are checked by executable models backed by
  the actual 32/64-bit SealableCounter acquire/release/reopen transitions.
  Representation lemmas relate raw counts and explicit permit counts to the
  abstract generation invariant. Seven unsafe ordering mutations must fail.
- These facts do **not** close the full composition rows above. In particular,
  the production reader retry loop, drain backend/striped observations, and
  subsystem allocation capabilities still require mechanical connections.

- Cache pin acquisition and release classification now use shared 32/64-bit
  transition expressions. Resident/flight anchor additions also use the checked
  acquisition path; the flight addition follows index publication and cannot
  rely on a fixed small pin count in the presence of concurrent readers.
  Release ordering is preserved. Three unsafe arithmetic mutations are rejected.
  A storage-backed ledger now conserves creator/resident/flight/lease fragments
  and observations, guards typed pointer reads, and withdraws memory only after
  pins and observations disappear. Production fragment creation and
  per-generation observation obligations remain open.

Earlier validation of the rotation/pin increment (2026-09-21, local macOS):

- All eight Verus targets pass (RRD: 87 obligations; Cache: 33, including
  shared kernel obligations). All 15 negative mutations are rejected at the
  verification stage, not merely by the parser/compiler.
- TCB audit includes `crates/xlfn/src` as well as the kernel and verification
  sources; zero `assume` and unapproved `external_body` findings.
- Kernel tests: 66 pass, including Loom. Cache tests with the cache feature:
  58 pass, one pre-existing test is ignored.
- Targeted cache `miri_` tests: two pass with Stacked Borrows and two with Tree
  Borrows (`nightly-2026-08-22`). This is not a full-cache Miri certification.
- Kernel and cache-feature library Clippy pass with warnings denied;
  formatting and whitespace checks pass. Remote CI/Windows not executed.

## Cache ownership increment (2026-09-21, local macOS)

- Cache Verus: 64 obligations pass. Four ownership mutations are rejected by
  verification (duplicate creator, reclaim with pins, reclaim with observations,
  wrong allocation instance), not parser/compiler rejection.
- Resident insertion now transfers one resident pin into one RAII entry before
  user Clone/Hash code can panic. This fixes double resident-pin decrement by
  the former outer rollback guard and inner owning entry. Quick lookup clones
  are non-owning snapshots; Moka comparator clones share one Arc-owned entry.
- Native cache/comparator tests: 76 pass, one ignored. Cache/comparator Clippy
  and the panic-boundary audit pass.
- The expanded Miri regression with benchmark comparators fails Stacked Borrows
  in crossbeam-epoch 0.9.21 `internal.rs:567`, reached through Moka maintenance.
  This is a failed validation, not covered by the earlier targeted Miri passes.
  The same all-backend regression reaches its assertions under Tree Borrows
  but fails the final leak check in crossbeam-epoch allocations. Neither
  all-backend Miri run passes; no leak/provenance checks were suppressed.
- The production-only Quick Cache regression passes both Stacked Borrows and
  Tree Borrows (one test each), with leak checks enabled.
- Final `just verus`: all eight targets pass, totaling 331 obligations; all 19
  negative mutations fail at the verification stage. `just verus-audit` passes
  its five scanner tests with zero project-source assumes or unapproved trusted
  bodies. These checks do not close the production composition rows above.

## Waiting registration increment

- `SealableCounter::mark_waiting` and its Loom adapter now instantiate the same
  macro-defined fetch_or/AcqRel/waiting-bit/active-mask expression as the
  executable DrainGate verifier. The verifier implements the primitive RMW
  contract instead of directly setting the waiting state in its register method.
- Both widths verify registration and wait-loop postconditions (DrainGate: 83
  obligations). Relaxed ordering, omitted waiting bit and fabricated zero
  observation mutations must fail verification. This checks correct primitive
  use; it does not verify the native atomic implementation/history.
- Striped observation stability and backend/lifetime composition remain open.

Validation: kernel native/Loom tests 66 pass; kernel all-target Clippy with
warnings denied, formatting, and TCB audit pass. All eight Verus targets pass
(333 obligations) and all 22 negative mutations are rejected. No Windows or
remote CI result is claimed for this increment.

## Striped observation increment

- Blocking and polled production drains, and the Loom blocking adapter, use
  one shared full-stripe registration/zero scan. Every stripe is registered,
  including stripes after an observed busy stripe. An idle result means all
  observed counts are zero; sums and their possible overflow are unnecessary.
- Verus verifies the same loop against both-width RMW backends, including
  preservation of other stripes and conjunction of every observed zero. A
  representation lemma derives zero per-stripe permits from the scan result.
- This does not assume a simultaneous snapshot of independent atomics: applying
  the result as a stable grace period still requires sealed/non-reopening
  interference composition, which remains open with idle-seal rollback.
- Mutation checks reject omitted final stripe, ignored busy stripe, and clearing
  an earlier busy observation when a later stripe is idle.

Validation: all eight Verus targets pass (343 obligations), including 93 for
DrainGate; all 25 negative mutations are rejected at verification. Kernel/Loom
66 tests, all-target Clippy, formatting and TCB audit pass locally.

## Idle-seal primitive increment

- Production and Loom instantiate one shared load/guard/compare_exchange body
  for idle-seal. The executable Verus primitive backend permits an arbitrary
  stale load sample and compares it to the actual current word, so the proof
  does not assume the reader/load/CAS race away.
- Both widths prove that success seals only an open idle counter, preserves
  waiting, and failure leaves the current word unchanged. Required Acquire load
  and AcqRel/Acquire CAS orderings are part of the shared expression.
- Production rollback uses a shared expression verified to reject non-idle or
  unsealed words and preserve waiting while clearing only sealed. A waiting bit
  set before rollback is retained; ordinary reopen still clears waiting.
- Four mutations (active counter seal, CAS without sealing, weakened CAS order,
  rollback clearing waiting) are rejected at verification. This closes these
  primitive expressions, not the multi-stripe rollback driver or atomic history.

Validation: all eight Verus targets pass (365 obligations), all 29 mutations
fail verification, and kernel/Loom 66 tests pass. Kernel all-target Clippy,
formatting, whitespace and TCB audit pass locally.

## Multi-stripe rollback driver increment

- Production `try_seal_if_idle` now instantiates the same indexed seal/rollback
  loops as Verus. The backend uses the previously shared CAS and undo expressions.
- In both widths, success seals every stripe at zero, while failure rolls back
  exactly the successfully sealed prefix. No failed or untouched stripe is
  undone. Waiting flags are preserved. Independent stale CAS samples are allowed.
- The driver proof describes sequential backend effects plus stale-CAS failure;
  concurrent registration or admission after a stripe is rolled back is not
  encoded as an interference step yet. In particular, exact input restoration
  is a backend theorem, not a claim that the live concurrent array is unchanged
  when the production operation returns false.
- Three mutations reject omitted undo, skipped final rollback, and undoing the
  failed stripe. The scan mutation anchor is scoped to its own loop after adding
  the new driver, so an ambiguous source match cannot silently weaken the gate.

Validation: all eight Verus targets pass (379 obligations), all 32 mutations
fail verification, and kernel/Loom 66 tests pass. Kernel all-target Clippy,
formatting, whitespace and TCB audit pass locally. Concurrent interference and
the upper-layer generation/retirement composition are still not closed.

## Sealed observation stability increment

- The rely relation enumerates unchanged observations/failed RMWs, waiting-bit
  registration, repeated seal, and successful acquire/release from the shared
  counter specifications. Reopen and idle-seal undo are explicitly excluded.
- Both widths prove that every such step preserves sealing and cannot increase
  the active count. Induction covers any finite history, so a sealed zero
  observation stays zero through an arbitrary number of later reader steps.
  The permit invariant then gives zero permits at the history endpoint.
- This is a per-stripe temporal lemma: different stripes may begin their
  histories at different observations. The actual owner must still provide the
  non-reopen interval and associate each sampled history with that stripe and
  generation. The lemma does not manufacture that ownership evidence or a
  simultaneous snapshot, and the cross-layer composition row remains open.
- Sensitivity checks mutate the rely relation to admit reopen or unchecked
  count increase. These are proof-model mutations, distinct from the existing
  production-shared control-flow mutations.

## Production drained-certificate lifetime and identity

- `DrainedGeneration` is no longer Copy or an unscoped array index. It borrows
  the issuing domain and transition state for a higher-ranked callback lifetime.
  The callback cannot return the certificate across a later generation reuse.
- Queue index extraction requires `index_for` with the issuing domain. Cache,
  handle binding, and handle topic retirement consumers now perform this check;
  a foreign certificate fails before any queue is detached. The unchecked index
  accessor is removed rather than retained as a bypass.
- A native test checks same/foreign domains. A compile-fail doctest confirms
  certificate escape is rejected specifically by the lifetime checker.
- These are Rust-enforced production connections, not new Verus theorems about
  issuance. The private issuer's drained-state preconditions and the full
  transition-lock/queue/permission composition remain open. Terminal drain-all
  paths remain a separate ownership obligation.

Validation: native kernel/Loom 67 tests and xlfn cache/bench-internals 427 tests
pass (8 pre-existing ignored). The certificate escape doctest fails compilation
for the intended lifetime reason. All-target xlfn Clippy, formatting, whitespace,
panic-boundary audit and TCB audit pass. No new end-to-end Verus claim follows
from these Rust lifetime/identity checks alone.

## Production transition-lock capability

- Blocking rotation helpers and the drained-certificate issuer now require an
  actual `parking_lot::MutexGuard`, not a plain mutable pending Option.
- Issuance and publish/reopen both validate that the guard belongs to this
  domain's transition mutex and that its pending generation is the requested
  one. The registration/reuse test now establishes that pending state explicitly.
- The callback-scoped certificate borrows this held guard. Thus certificate
  lifetime and generation reuse now meet at the same production mutex; mutex
  exclusion remains the primitive TCB contract.
- Drain completion/stripe history and tracked allocation-permission composition
  are still required. This lock-capability change alone does not establish them.

Validation after guard-gated publication: kernel/Loom 67 tests pass. The full
xlfn run reports 426 passed, 1 failed and 8 ignored: diagnostics sink construction
observed two events instead of one. This failure is under investigation; earlier
427-test passes are not substituted for the final run. All-target xlfn Clippy,
formatting, whitespace and TCB audit pass. The prior callback-escape doctest
also passes with guard-typed issuance. Remote/Windows checks were not run.

The diagnostics failure was a global-sink test-isolation defect: its CountingSink
counted all concurrent reports, although the test intends to check delivery of
its own event after failed replacement. The test now selects its unique event
ID and still requires exactly one delivery. After this repair the complete xlfn
run passes 427 tests (8 ignored), and all-target Clippy passes. No production
diagnostics behavior was changed.

## Terminal drain to queue extraction

- `RotatingReadDomain::seal_and_wait` returns `ClosedDomain`, issued only after
  the shared closure and both-generation waits. Its borrowed identity can
  authorize both queues only for that same domain. Unlike rotation, this
  certificate remains valid after unlock because closure is permanent.
- Cache's final `drain_all` now requires this certificate; its Drop path retains
  the certificate across resident-index clearing and then detaches the queues.
  The cleanup-only registration test now explicitly seals before drain-all.
- Handle binding's ordinary `take_generation` requires the callback certificate
  itself, rather than an unchecked integer. Its terminal path and Handle topic's
  terminal path extract indices through the closed-domain certificate. Existing
  batch allocation reuse and per-queue accounting are preserved.
- Same/foreign domain checks cover both certificate forms. Two compile-fail
  doctests cover callback escape and a terminal certificate outliving its owner.
- This strengthens production caller preconditions. The Verus allocation token,
  reader-history association, and issuance proof composition remain open.

Validation: kernel/Loom 67 tests, xlfn cache/bench-internals 427 tests (8 ignored),
two lifetime doctests, all-target Clippy, formatting and TCB audit pass locally.

## PublishedOwner permission and claim correction

- The old PO-1 theorem only equates numeric identities; it does not establish
  linear allocation ownership. It is renamed and documented as snapshot
  agreement. The native into_box/Drop comments no longer claim full Verus proof.
- `permission.rs` adds an opaque owner of a tracked initialized `PointsTo<T>`.
  Adoption transfers that resource; pointer reads preserve it; borrowed access
  requires its matching initialized invariant; recovery consumes the owner.
  This uses full pointer provenance, not non-null or numeric ID as permission.
- Two proof mutations reject mismatched pointer adoption and unguarded borrowing.
  These are model-permission checks, not production expression mutations.
- The unused callable unsafe `PublishedOwner::release_inner(&mut self)` hook is
  removed; Drop directly performs its existing Box recovery/destruction. This
  eliminates a redundant unsafe entry point without changing deallocation order.
- Native Box/unsized/Drop adapters and cross-layer temporal reader permissions
  remain open. The new permission proof must not be presented as that connection.

Validation: PublishedOwner Verus passes 10 obligations; both new permission
mutations fail verification. Its two native tests pass; the destruction/recovery
Miri regression passes in both Stacked and Tree Borrows. Kernel Clippy,
formatting, whitespace and TCB audit pass.

## Shared retirement registration

- `RotatingReadDomain::register_retired` owns the selection/queue-lock/recheck/
  retry/append control flow shared with Verus. Cache, handle binding and handle
  topic now call this helper. Their payload is moved into a FnOnce registration
  callback; a stale selection releases the queue and retries before calling it.
- The executable backend permits arbitrary generation changes before queue-lock
  acquisition. Append requires the held queue to match the reobserved current
  generation. The same queue excludes publication while held under the barrier
  contract. Safety is proved without a fairness/termination assumption.
- Three shared-control-flow mutations reject skipped recheck, retry with the
  stale lock retained, and releasing the queue before append.
- Production source correspondence: Cache uses `pending_reclaims` for both
  registration and its publication barrier; Handle binding uses `pending`;
  Handle topic uses `pending_reclaims`. These callback-to-queue identity facts
  are inspected in source, not yet encoded in tracked Verus queue permissions.
  That remaining identity/payload/reader-permission composition is still open.

Validation: RotatingReadDomain Verus passes 93 obligations and all 10 shared
rotation/registration mutations are rejected. Kernel/Loom 67 tests, xlfn
cache/bench-internals 427 tests (8 ignored), all-target Clippy, formatting,
whitespace and TCB audit pass locally.

## Typed cache retirement payload and batch phase

- `CacheLookupDomain<V>` and `ReclaimEntry<V>` retain the actual `CacheNode<V>`
  pointer type from pin release through retirement queues and Box recovery.
  Reclamation no longer casts a `*mut ()` back into a caller-selected V.
- ReclaimEntry remains non-Copy/non-Clone, and Send now requires V: Send instead
  of erasing the payload's transfer restriction. A trait/layout regression test
  checks those properties without increasing the pointer/weight entry size.
- PendingEntries and ReclaimEntries are distinct types. Domain-validated drain
  methods wrap detached queues as reclaimable batches; the safe batch reclaimer
  accepts only the latter. Empty batches and merging preserve this distinction.
  The wrapper is transparent and retains SmallVec storage/allocation behavior.
- These are production type connections to the typed permission model. The
  module's batch constructors and final-pin/reader/certificate correspondence
  still need the tracked Verus composition; the wrappers alone do not prove it.

Validation: xlfn cache/bench-internals 428 tests pass (8 ignored). Production
cache Miri tests pass three each in Stacked and Tree Borrows with leak checks
enabled. All-target Clippy, formatting, whitespace and TCB audit pass. This does
not change the earlier failed Moka-comparator Miri evidence.

## Final-pin retirement ownership

- Production ReclaimEntry construction is centralized in an unsafe live-pin
  transfer helper. Only its final release branch creates an entry. Resident,
  lease/creator release and failed-lookup pin rollback all use it. Queue enqueue
  consumes the entry instead of accepting an arbitrary raw pointer and weight.
  Sentinel construction is test-only and never used by a real node reclaimer.
- The tokenized cache machine now issues a unique option-sharded retirement
  ticket at final release. Shared 32/64-bit release wrappers return that ticket
  exactly on LastPin and bind it to the same instance and memory permission.
  Recovery consumes the ticket after observations end, then withdraws memory.
- Removing the old explicit zero-count reclaim guard is no longer an unsafe
  mutation: the retirement ticket itself now implies zero pins. The negative
  check instead permits ticket issuance from a nonfinal pin, which is rejected.
- Native entry construction and the tracked ticket have aligned roles; the
  production-to-ghost ownership representation, queue identity and domain
  observation discharge still require the final composition proof.

During validation, the full test suite exposed an independent test-fixture
ownership defect: cleanup of an unopened, locally quarantined Runtime could
close another runtime's global ingress without owning its module lease.
Test-only cleanup now requires the lease, with a deterministic two-fixture
regression. Production runtime behavior is unchanged. The repaired full run
passes 429 tests (8 ignored); Cache Verus passes 65 obligations and all four
ownership mutations are rejected.

Production cache Miri passes three tests in each of Stacked and Tree Borrows.
All-target Clippy, formatting, whitespace and TCB audit also pass for this
increment. The full composition objective remains open.


## Allocator capability preserved through retirement

- `PointsTo<T>` alone does not grant deallocation rights. The shared
  `published_owner/src/heap_permission.rs` now pairs it with `Dealloc`, matching
  address, size, alignment and provenance. Zero-sized values need no allocation.
- PublishedOwner adoption/recovery and Cache pin storage/retirement tickets
  preserve this compound resource. Borrowing exposes only the matching memory
  guard. `free_uninitialized` consumes both resources against the actual vstd
  deallocation primitive contract; no project-local trusted body was introduced.
- Three negative mutations removing layout, provenance or nonzero-allocation
  capability requirements fail at the verification stage. Existing owner, cache
  arithmetic and cache ownership negative checks also pass with the shared module.
- PublishedOwner verifies 14 obligations and CacheLease 69. These results do not
  establish native Box allocation/recovery, initialized-value destruction,
  unsized layouts, or domain/queue observation discharge. Those adapters and
  cross-layer composition remain open; vstd allocator semantics remain TCB.

Validation of allocator-capability increment (local macOS aarch64):
`just verus` passes all eight targets (414 obligations, zero errors) and all
42 negative mutations fail at the verification stage. `just verus-audit` passes
five scanner tests with zero assumes/unapproved external bodies.
`git diff --check` passes. This increment changes proofs and documentation;
no new native Rust, Miri, Windows or remote CI result is claimed.


## Production reader entry shared with the counter-backed proof

- Borrowed and owned `RotatingReadDomain` entry now instantiate the same
  `enter_reader!` loop as Verus. The Loom reader/rotation model uses it too.
  Selection is repeated after rejection; construction of the returned permit
  occurs only after successful acquire on that selected generation.
- The executable backend reuses `reader_acquire_selected` and the production
  32/64-bit counter transition. Arbitrary invariant-preserving snapshots may
  intervene before each attempt. Success returns the acquired current generation;
  an error requires a closed observation and no acquired permit. Overflow has
  no returning path (modeled divergence for native abort), not a retry result.
- Removing acquisition or returning the other generation's permit fails Verus
  obligations. All 12 rotation negative cases pass; the rotation target verifies
  107 obligations with zero errors. The TCB audit also passes.
- Snapshot interference is an overapproximation, not a proof that native atomic
  histories preserve the generation invariant. Stripe identity, owned permit
  storage lifetime and observation/allocation capabilities still need composition.
  No termination, fairness, or entire admission-to-reclaim theorem is claimed.

Native validation of reader entry: all 67 kernel tests pass, including Loom;
kernel library/test Clippy with warnings denied, formatting and diff checks pass.
The 11 RotatingReadDomain `miri_` tests pass under both Stacked and Tree Borrows
on nightly-2026-08-22. Both runs retain the parking_lot_core integer-to-pointer
provenance warning; this is not strict-provenance certification. Windows and
remote CI were not run for this increment.


## Linear admission-to-observation resources

- `drain_gate/src/permits.rs` replaces numeric-only reasoning at this boundary
  with instance-bound, non-duplicable admission fragments and a conserved active
  count. Both-width wrappers issue/consume fragments using the shared production
  acquire/release expressions. The fragment proves a positive active count; a
  matching zero observation excludes any such live fragment.
- `cache_lease/src/scope_ownership.rs` composes a borrowed admission fragment with
  an actual cache observation token. Creation needs the node's resident fragment;
  ending the observation consumes its token and decrements the node observation
  ledger. Typed borrowing delegates to the existing storage-backed PointsTo guard.
  The reference lifetime retains the scope's borrowed permit. A permit may back
  multiple node observations; no observations-to-permits cardinality bound is used.
- This establishes resource and borrowing composition, not the native source of
  the resources. Node/domain registration, stripe and generation instance mapping,
  batching of scope-exit observation discharge, and drain-certificate-to-queue
  withdrawal remain open. In particular, a scope token is not evidence that an
  arbitrary node belongs to that domain. These requirements must not be replaced
  with pointer non-nullness or added as unstated assumptions.
- DrainGate verifies 159 obligations and CacheLease 105 with these imported
  modules. The TCB scanner tests pass with zero assumes/unapproved external bodies.

Validation of the linear admission/observation increment: all eight Verus targets
pass (478 obligations, zero errors). The full gate rejects 48 mutations; the
additional counter-release/permit-ledger mutation added while that run was in
flight also passes separately (49 total). Mutations are required to fail at the
verification stage, not on Rust signature errors. The initial retain-versus-remove
mutation changed a generated token API and was not counted as a proof rejection;
the final checks preserve signatures and test conservation plus caller wiring.
TCB audit and diff checks pass. This increment changes proof resources and their
composition only; native/Miri/Windows results are not newly claimed.


## Immutable node domain and scoped membership

- The storage-backed cache machine now fixes a set of gate instance identities
  in a constant `domain` field at allocation. This models membership in the
  domain across both generations and all stripes, not a permanent assignment of
  the node to the reader's current generation. No transition can rebind this set.
- `initialize_node` consumes initialized `HeapPermission<T>`, deposits it into
  actual tokenized storage, and returns the node ledger plus its sole creator
  pin. Postconditions preserve the assigned domain and establish zero initial
  observations and no retirement. Empty generated token collections are discarded;
  initialized memory and the creator capability are retained in the result.
- `ScopedObservation::observe` now requires the borrowed gate permit to belong
  to that immutable domain. The opaque tracked observation stores that association
  as a type invariant; `borrow_scoped` checks the node's domain and proves the
  borrowed permit is one of its members while obtaining the real PointsTo guard.
- Production `CacheNode::domain` is initialized from the owning CalculationCache
  and has no mutation path, but mapping that native domain's concrete generation/
  stripe addresses to this gate-instance set is still open. The new initializer
  consumes proof-level memory ownership; it does not prove the native Box::new
  adapter or authorize arbitrary inferred membership. Queue generation coverage
  and retirement-to-drain composition remain required.

Validation: CacheLease verifies 106 obligations with zero errors; all eight
ownership/domain mutations and three pin-kernel mutations are rejected. The
negative harness recognizes Verus's declared-type-invariant failure diagnostic
in addition to its existing verification diagnostics; Rust/parser failures remain
excluded. TCB audit and whitespace checks pass. Other targets and native tests
were not rerun for this proof-only increment.


## Rotation admission resources composed with Cache observations

- Rotation now imports the same DrainGate linear admission module as its
  ownership backend. `GenerationLedgers` fixes the two instance identities and
  relates their active fragments to decoded raw counters. Both-width
  `acquire_selected_with_permit` calls the existing shared-counter-backed
  selected acquire and issues a fragment only for its successful selected gate.
- `release_selected_permit` consumes that gate's fragment while executing the
  existing counter-backed release; both gate counts and identities are preserved.
  An idle generation excludes a matching outstanding fragment. The selected
  acquire contract now explicitly preserves the other generation's counter.
- Cache imports the rotation proof source itself and uses its admission type,
  rather than independently instantiating the same token declarations. Its two
  `excludes_drained_generation` lemmas directly compose the rotation zero
  observation with the borrowed permit in the storage-backed Cache observation.
- The unused native `RotatingReadOwnedPermit::release_inner(&mut self)` hook is
  removed. Drop directly performs the sole raw release, matching consuming
  ownership and removing an unnecessary repeatable internal release API. No
  ordering or owner-lifetime precondition changes are made.
- This is still conditional on a matching generation ledger at the RMW boundary.
  Native striped counter/instance representation, interference history across
  the full reader loop, initial ledger construction, and queue-to-reader coverage
  are open. Generation-zero exclusion does not by itself show that every node
  observation belonged to that drained generation or discharge the entire cache
  observation ledger. These are required before claiming safe queue withdrawal.

Validation of the rotation/Cache resource composition: `just verus` passes all
eight targets (596 obligations including reverified imported modules), and all
56 negative mutations are rejected at verification. Rotation: 127; Cache: 203.
Kernel tests: 67 pass, including Loom. Clippy with warnings denied, formatting,
TCB audit and diff checks pass. The owned-permit immediate-reclamation Miri test
passes in both Stacked and Tree Borrows; parking_lot_core's provenance warning
remains. No Windows/remote CI result is claimed.


## All-stripe scan and interference composed with Cache ownership

- DrainGate's proof modules are now importable as one module hierarchy.
  Rotation imports DrainGate itself; Cache imports Rotation. Counter transitions,
  linear admission fragments and stripe scan proofs therefore share definitions
  and types within the Cache verification target.
- `stripe_ownership.rs::StripeLedgers` maps each raw-vector position to a fixed,
  distinct gate instance and its active token. `scan_with_ledgers` calls the
  existing production-shared registration/scan and proves its boolean result is
  exactly the conjunction of zero active tokens over all positions.
- Cache `scan_while_observed` directly executes that scan while retaining the
  actual scoped observation's permit. Given the matching stripe ledger, returning
  idle is impossible. There is no numeric-only substitute for the borrowed permit.
- Per-stripe histories may start at different zero-observation times. The
  `drained_histories` condition requires sealed zero samples followed by the
  existing rely relation over shared counter transitions (excluding reopen/undo).
  `histories_exclude_permit` relates their final states to active tokens and
  excludes any matching live fragment. Cache imports this result to exclude its
  matching scoped observation after those histories.
- These functions do not establish the native source of the initial/final
  ledger correspondence or prove transition-lock/atomic-history instrumentation.
  The scan adapter still models RMW samples; the history lemma supplies stability
  only under its stated rely relation. Retirement queue coverage and per-node
  observation discharge remain open, as does the mapping from native array
  addresses to proof instance identities. No actual unsafe call is declared
  justified solely by a manually supplied zero count.

Validation of stripe/history ownership composition: all eight `just verus`
targets pass (878 obligations, including repeated imported modules), and all
61 negative mutations fail at verification. DrainGate: 167; Rotation: 262;
Cache: 342. TCB audit and diff checks pass. No native source changed in this
increment; no new native/Miri/Windows or remote CI result is claimed.


## Linear retirement payload registration and recovery

- The existing `Registration<P>` backend now owns typed payload queues. The
  production-shared `register_retired!` loop preserves both queues through all
  stale-selection retries and appends the moved payload exactly once to the
  revalidated current generation. The other queue and both prior prefixes are
  preserved. This replaces the earlier registered-generation-only result.
- Cache `RetiredNode<T>` carries the actual linear final-pin retirement fragment,
  typed pointer and immutable domain. Both-width `release_to_entry` executes the
  shared pin kernel's ownership wrapper and creates this entry iff the released
  pin was last. Memory/allocator permission remains in node storage until recovery.
- `register_retirement` instantiates the shared queue algorithm with that concrete
  payload, preserving queue domain membership and exactly one append. It cannot
  substitute a numeric allocation ID for the owned retirement fragment.
- `recover_retired` consumes the entry's fragment to recover the original
  HeapPermission from node storage. It still requires the matching zero-pin,
  zero-observation ledgers; dequeue alone does not authorize recovery.
- Queue Vec operations are the executable proof backend for production SmallVec
  operations. Native closure/container wiring remains a representation obligation.
  The payload proof establishes no retirement epoch cutoff: proving that the
  drained queue covers all older/current observations, and thus supplies zero
  observations for its nodes, remains required. No observation count is reset
  to zero by the queue algorithm or assumed as a consequence of registration.

Validation of typed retirement queue composition: all eight `just verus` targets
pass (883 obligations including imported modules), and all 67 negative mutations
are rejected at verification. Rotation: 262; Cache: 347. TCB audit and diff
checks pass. This increment changes executable proof backends and resource
composition; native Rust, Miri, Windows and remote CI were not rerun.


## Production-shared drain certificate domain authorization

- Both native `DrainedGeneration::index_for` and `ClosedDomain::indices_for`
  now call `authorize_domain!`. Verus instantiates the same pointer comparison
  and payload authorization expression. The contract uses pointer-address
  equality (not a claim that provenance or allocation ownership follows from
  numeric equality), and preserves the exact authorized generation payload.
- No project trusted pointer function was added. Native thin-pointer equality
  replaced `ptr::eq`; these two certificate sites refer to live AtomicUsize
  fields. The executable Verus check verifies the comparison itself.
- Rotation now retains its domain pointer across modeled mutation methods.
  Both-width opaque borrowed `Drained` proof certificates require that domain,
  a held transition, the matching pending generation and an actual idle state.
  Their authorization methods use the shared expression and preserve the idle
  and pending facts for the returned index. Terminal address authorization
  verifies the exact [0, 1] payload separately.
- This does not prove native certificate issuance's complete lock/history
  representation. The proof certificate borrows a model state, whereas native
  code borrows the transition guard and allows other-generation reader activity.
  Connecting that frame/stability condition and actual MutexGuard ownership
  remains required. An authorized index still does not prove retirement queue
  coverage of all node observations or permit arbitrary Box recovery.

Validation of shared certificate authorization: `just verus` passes all eight
targets (895 obligations including imported modules), with all 71 mutations
rejected at verification. Rotation: 268; Cache: 353. Kernel library tests: 67
pass; both lifetime compile-fail doctests pass. Kernel Clippy with warnings denied,
formatting, TCB audit and diff checks pass. No Windows or remote CI run is claimed.


## Shared certificate-to-queue detachment in Cache and Handle

- Native `DrainedGeneration::take_queue` and Verus detachment instantiate
  `take_authorized_queue!`: authorize domain/index, acquire that queue guard,
  then invoke the consuming take operation once. Authorization failure invokes
  neither callback. The native foreign-domain regression now checks those
  side effects, as well as one successful lock/take pair.
- Cache, Handle binding records and Handle topic retirement queues now call
  this method for ordinary generation detachment. Cache metrics retain their
  previous queue-lock scope; binding/topic lock lifetimes also stay unchanged.
- The executable registration backend now preserves a queue-owner pointer.
  Its detachment methods prove exact payload transfer, an empty selected queue,
  an unchanged other queue, and no mutation on failed authorization. Cache
  instantiates the algorithm with real linear RetiredNode payloads and retains
  their domain membership. Verus uses supported mem::swap with an empty Vec for
  its container backend; no local trusted spec for mem::replace/take was added.
- Container and MutexGuard implementation semantics remain backend boundaries.
  The shared callback protocol does not prove the caller supplied the correct
  native mutex or the pointer-to-ledger representation. Native terminal drain-all
  and the queue's coverage of every relevant observation still need composition.
  Handle uses this shared production path, but its binding/topic memory and debt
  ownership proofs are not thereby completed.

Validation of shared detachment: all eight Verus targets pass (905 obligations
including imported modules), and all 73 negative mutations fail verification.
Rotation: 272; Cache: 359. Native xlfn tests with cache/handles/bench-internals:
429 pass, eight ignored; kernel: 67 pass. Both crates' library/test Clippy with
warnings denied, formatting, TCB audit and diff checks pass. The cache+handles
`miri_` suite passes 26 tests each under Stacked and Tree Borrows on
nightly-2026-08-22, with the parking_lot_core provenance warning retained. The
Miri run excludes benchmark-comparator backends; it does not supersede their
previously recorded failure. Windows and remote CI were not run.


## Shared terminal detachment of both queues

- `ClosedDomain::take_queues` shares authorize/lock-zero/lock-one/take-both
  control flow with Verus. Foreign-domain authorization runs no callbacks;
  the successful native regression checks index order and one take invocation.
- Cache, Handle bindings and Handle topics now use it for terminal detachment.
  All acquire queue locks in 0/1 order before taking payloads. Cache and bindings
  combine batches after guard destruction; binding debt accounting is retained.
  Topics retain their previous two-lock discipline. This intentionally extends
  paired locking to the other terminal paths; it does not affect admission paths.
- The executable queue backend tracks both held guards and proves that both
  exact payload sequences are returned and both queues become empty. Cache
  instantiates this with linear retirement entries and preserves their domain.
- Both-width terminal proof certificates require closed admission, both sealed
  generations and both idle states; domain authorization preserves the exact
  [0, 1] selection. These are conditions on the borrowed rotation model. Native
  all-stripe history/identity and model-guard representation still require the
  previously listed connections; no native facts are manufactured by issuing
  the proof certificate. Completing terminal payload transfer does not complete
  ordinary retirement epoch coverage, native Box recovery, or Handle debt proofs.

Validation of terminal detachment: all eight Verus targets pass (923 obligations
including imported modules), and all 76 negative mutations are rejected.
Rotation: 280; Cache: 369. Native xlfn/kernel: 496 pass, eight ignored; Clippy,
formatting, TCB audit and diff checks pass. The targeted deferred-seal, topic-close
and zero-budget-cache Miri tests pass five cases per mode (Stacked and Tree).
These targeted runs do not replace broader earlier Miri evidence or the separate
benchmark-comparator failures. Windows/remote CI were not run.


## Exclusive resource required for drained certificates

- Ordinary proof certificates now borrow executable rotation state and a real
  `vstd::rwlock::WriteHandle`. Issuance checks the exact expected lock and its
  domain predicate; a boolean locked flag alone no longer suffices.
- The positive resource path acquires exclusive state, checks pending/idle,
  issues the borrowed certificate, detaches the authorized queue and consumes
  the handle to restore state. Cache instantiates it with linear retired nodes.
- Four additional SMT mutations reject a foreign lock handle, a foreign lock
  domain, issuance before idle, and restoring state under another domain lock.
  `check_transition_borrows.py` separately verifies a clean baseline and requires
  specific E0505 borrow errors when release is moved before certificate use.
  General compiler failures do not count as passing either negative gate.
- This backend locks the complete Rotation state. Native transition guards only
  serialize transitions; reader atomics can change independently. The native
  interference frame and guard representation still require proof. The library
  write handle needs explicit release and does not establish native Drop/unwind
  behavior. No native Mutex implementation or queue observation coverage is
  inferred from this resource backend.

Validation: all eight Verus targets pass, totaling 941 obligations including
reverified imports (Rotation 288, Cache 379). All 80 SMT mutations are rejected;
the separate live-certificate borrow regression passes. TCB audit passes with
zero project assumes or unapproved external bodies. Native production code was
unchanged in this proof increment, so the preceding native/Miri results remain
the applicable evidence; Windows and remote CI were not run.


## Conserved observation coverage and zero-observation recovery

- `ObservationLedger` owns the unique node observation counter and a tracked map
  of every `ScopedObservation`, each retaining its borrowed gate permit. Private
  fields prevent substituting a list of copied IDs for those resources.
  Initialization starts from the real allocation constructor; observe and end
  preserve exact count/map conservation. One permit can still cover many nodes
  or repeated observations, without assuming a one-to-one admission count.
- `borrow_covered` borrows the actual memory guard through a ledger entry.
  `zero_after_histories` selects an actual retained permit if the map is nonempty;
  the matching sealed-zero stripe history excludes that permit. Conservation
  derives the node observation count as zero. `recover_after_covered_drain`
  invokes this proof before consuming the final-pin ticket and withdrawing heap
  permission; it does not accept zero observations as a precondition.
- Coverage here requires every gate in the node domain to be included in the
  supplied histories. It is suitable for whole-domain quiescence, not a claim
  that one ordinarily drained generation covers both generations. The native
  terminal certificate-to-stripe mapping, ordinary retirement publication cutoff,
  and native index/scope/ledger representation remain open. The real Cache
  `seal` drains the domain and `drain_all` detaches both queues, but that source
  correspondence alone is not a mechanical proof of these remaining adapters.

Validation: the affected Cache target passes 391 obligations including imported
modules. Its 19 ownership/composition mutations are rejected, including four new
cases: missing live entry, missing count decrement, broken count conservation,
and an uncovered node gate. TCB audit passes with zero project assumes and zero
unapproved external bodies. Only proof code and verification tooling changed;
previous native/Miri results are not presented as new runs for this increment.


## Frozen observation coverage across generation reuse

- Final shared pin release now has a resource wrapper that freezes the exact
  node observation ledger when it creates the final-pin retirement entry.
  Frozen ledgers permit reads and observation completion but forbid additions.
  Completion preserves their narrowed coverage; no thaw or coverage-expansion
  operation exists.
- A sealed-zero history can remove gates from frozen coverage only by excluding
  actual retained permits. Count and entries are conserved: narrowing does not
  discard observations or reset the count. Gate reuse cannot add those gates
  back to the retired node's coverage.
- `narrow_before_rotation` uses the existing Rotation and GenerationLedgers:
  no pending plus the rotation invariant entails that the previous generation
  is idle. With the exact node/domain gate identities it excludes that previous
  generation and retains only current-generation observation coverage.
- `narrow_and_begin` then runs the production-shared begin/publication expression.
  Strengthened seal/publish/reopen contracts prove active counts are preserved,
  so the same linear gate ledgers match the new rotation state. Coverage now
  names its pending generation. `recover_after_pending` derives zero observations
  from that generation's idle result before withdrawing the real heap permission.
  The reopened current generation may have active readers.
- These are resource-level temporal connections, not native adapter completion.
  The scalar generation proof still needs the actual stripe/address/history
  mapping; every detached native queue payload must be connected to the frozen
  ledger narrowed before its publication cutoff. Native index observation and
  guard/atomic interference remain open. The shared publication call is used by
  this proof path, but production does not contain the ghost observation map.

- A positive borrow-lifetime witness creates an admission, observes a node,
  completes the observation, consumes the permit and returns the same node's
  zero-observation ledger. `rebind_empty` changes only the borrow lifetime after
  conservation proves the map empty; domain, coverage, frozen state and the
  unique counter token remain unchanged. This avoids keeping ended permits
  borrowed merely through the ledger type's lifetime parameter.

Validation: `just verus` passes all eight targets, including Rotation 294 and
Cache 410 obligations. The subsequent lifetime API/witness passes a focused
Cache verification at 412 obligations; the added lifetime-erasure mutation is
checked separately. Across the full gate and that focused follow-up, 93 SMT
mutations are rejected, including 28 Cache ownership/composition cases; the
separate transition-certificate borrow check also passes. TCB audit and diff
checks pass. No native production code changed in this increment, and no new
native/Miri, Windows, or remote CI run is claimed.


## Handle destruction-completion debt matches the production interval

- Corrected the previous Handle model equation `debt = pending_0 + pending_1`.
  Production removes a batch from a queue before running destructors, and debt
  remains outstanding throughout that interval. The model now includes
  `reclaiming_bindings`; detachment preserves debt and completed destruction
  discharges it. Close begins without erasing readers, and successful final
  completion derives zero debt from observed empty queues and no in-flight work.
- Production `HandleReadDomain::reclaim` now invokes the same macro as the
  32/64-bit executable backend: destruction return, debt discharge, completion
  lock, notification, unlock. Release ordering and the user-destructor lock
  boundary are preserved. A linear batch-instance receipt prevents reuse or
  discharge with a foreign batch, and the result refines the corrected model.
- The backend's destructor effect is explicitly modeled by a lexical ownership
  scope. Verus does not support explicit `core::mem::drop` here; no trusted
  specification was introduced to bypass that limitation. Native generic Drop,
  raw Box recovery, queue-to-batch ownership, and atomic debt representation are
  not proved by this increment. ObjectArena's existing native panic containment
  remains part of the actual destruction path.

Validation: Handle Verus passes 37 obligations, including both executable widths.
Nine new negative mutations fail proof obligations; compiler errors are rejected
by the harness and do not count as proof sensitivity. Native Handle tests pass
116 cases with one dedicated Shuttle test ignored. The in-flight-destructor
regression passes under Stacked and Tree Borrows on nightly-2026-08-22. Clippy
with warnings denied, formatting, TCB audit and diff checks pass. Other Verus
crates were unchanged; their preceding full-gate evidence remains applicable.
Windows and remote CI were not run.


## Shared checked Handle debt and queue-counter transitions

- `handle/domain/counters.rs` supplies identical add/subtract transition
  expressions to production and Verus for 32/64-bit words. Successful addition
  and subtraction preserve exact mathematical counts. Overflow and underflow
  produce FailStop, including correct success at exact capacity and final zero.
- Handle debt/queued mutations now feed those kernels to AtomicUsize::try_update;
  invalid counts abort instead of wrapping. Debt discharge retains Release
  success ordering and Relaxed failure ordering. Queued hints remain Relaxed.
  This changes cold retirement/maintenance RMWs from fetch arithmetic to checked
  update loops; it does not change reader admission or public APIs.
- The destructor receipt proof uses the shared subtraction kernel. Conditional
  finite-width enqueue/detach bridges connect raw counter results to the debt
  model, preserving in-flight destruction debt and proving subtraction is valid
  for the selected batch. Concurrent native intermediate-state representation
  remains open: a queue transfer and its counter update are separate operations.
  The boundary bridges do not assert instantaneous queue-length equality during
  those intermediate states or prove the AtomicUsize implementation.

Validation: Handle Verus passes 46 obligations. All 15 Handle negative mutations
are rejected, including four arithmetic and two count-correspondence mutations.
Native Handle tests pass 116 cases with one dedicated Shuttle case ignored;
Clippy with warnings denied, formatting, TCB audit and diff checks pass.
The three deferred-seal Miri regressions pass under both Stacked and Tree Borrows.
The actual 32-bit adapter compiles with cargo check for i686-pc-windows-msvc;
this is a cross-compilation check, not Windows execution or remote CI evidence.


## Native drained Handle batches retain their completion owner

- Split the pending SmallVec alias from opaque `DrainedBindings`. Nonempty
  batches are built only after ordinary or terminal certificate authorization;
  empty batches need no drain. Each batch borrows its HandleReadDomain and owns
  its records, with no Clone/Copy or raw-queue reclamation entry point.
- Drop empties the wrapper before executing the existing shared destruction
  completion tail against the borrowed domain. Explicit reclaim consumes the
  wrapper, but ordinary scope exit also discharges completed destruction debt.
  Existing call sites transfer batches out of queue/transition guards before
  destruction. This changes cleanup ownership without adding an unsafe block.
- Merge checks owner identity before payload movement. It transfers all records
  and leaves the source empty, so the source's Drop cannot decrement debt early.
  The same macro is instantiated by the Verus generic Vec backend, proving
  exact payload concatenation and preservation on rejected ownership.
- The native regression retires records in both generations, detaches and merges
  their batches, observes queued=0/debt=2 while payloads remain alive, then uses
  ordinary Drop and checks both payloads destroyed exactly once and debt=0.
- Native certificate-to-wrapper, SmallVec and arbitrary Drop refinement remain
  open. The shared merge proof and native borrow checks do not themselves
  complete the binding/object memory-permission or atomic-interference proofs.

Validation: Handle Verus passes 47 obligations and all 18 negative mutations
fail verification. Native Handle tests pass 117 cases, with one dedicated
Shuttle case ignored. The handles-only feature configuration's complete `miri_`
filter passes 24 tests in each mode (Stacked and Tree Borrows), including the new
batch-merge/drop regression and existing reentrant-destruction cases. The
parking_lot_core integer-to-pointer provenance warning remains visible and is
not suppressed. i686-pc-windows-msvc cargo check, Clippy with warnings denied,
formatting, panic-boundary audit, TCB audit and diff checks pass. The cross-check
is not Windows runtime or remote CI evidence.


## Handle certificates transport the same initialized heap-owner payload

- The Handle verifier imports the actual rotation proof modules, not a new
  certificate model. Batch fields are private; nonempty construction calls
  ordinary or terminal shared detachment and preserves the exact queue payloads.
  Empty construction and checked merge are the remaining construction paths.
- Batches borrow a DomainOwner, explicitly separating completion-owner identity
  from the embedded rotation identity. The queue must match that embedded
  identity before its payload can be attributed to the completion owner.
  The native mapping of these two addresses remains a representation obligation.
- BindingOwner adopts an actual initialized HeapPermission through the existing
  LinearOwner adapter and retains its domain borrow. The shared registration
  loop and typed ordinary/terminal detachment preserve those same owned values.
  BoundCompletion consumes the certified batch and retains its owner borrow
  through the shared completion tail. The typed complete_batch wrapper executes
  that path with BindingOwner payloads.
- This does not manufacture a proof of native Drop/Box deallocation or establish
  native raw-reader/arena ownership. The destructor effect remains the explicitly
  documented model boundary, and the native BindingRecord/SmallVec/address
  representation still needs connection.

Validation: Handle passes 366 obligations, including reverified imported rotation
and initialized-owner modules; this is not 366 new independent proofs. All 24
Handle negative mutations pass across the full script and the focused added
owner-identity check. New mutations cover foreign completion-owner binding,
discarded payload, exchanged terminal generations, mismatched heap pointer,
foreign-domain registration and omitted completion. TCB audit and diff checks
pass. Native production code was unchanged in this proof increment; the prior
117 native Handle tests and 24-per-mode Miri evidence were not rerun here.


## CallScope ownership is retained through native reader witnesses

- HandleDomainWitness now borrows the actual CallScope; its constructor is
  private to the scope code that finds or retains admission. The test-only
  standalone witness borrows its owning permit. Removed the unsafe constructor
  that relied only on a raw domain pointer and PhantomData.
- BindingReadLease stores the scoped witness for its entire lifetime. Standalone
  permit ownership is confined to tests. Domain identity is checked before
  loading a binding snapshot as before.
- The production permit store instantiates the same generic insertion expression
  as Verus. Exact sequence conservation proves both old and new admissions
  survive Single-to-Multiple promotion and subsequent growth. The proof also
  instantiates the store with actual nonduplicable gate-permit tokens.
- Promotion allocates before moving existing ownership out; Multiple appends in
  place. The old replacement-first code could unwind with earlier admissions
  absent if allocation panicked. This proof covers normal return; Rust Vec's
  allocation/unwind semantics remain a library boundary.
- A native regression keeps the first Handle and witness alive while adding
  seven more domains and retiring their bindings. It observes no destruction
  during scope, then exactly eight destructions and zero debt after scope exit.
- These connections do not close native RefCell-to-ledger, domain/gate address,
  raw binding/arena or independently changing atomic representation. A borrowed
  scope root plus append-only behavior must not be described as a Verus proof
  of the complete native reader adapter.

Validation: Handle Verus passes 368 obligations, including imported rotation and
heap-owner modules. All 27 Handle mutations fail at the verification stage.
Native xlfn library tests with cache/handles/bench-internals pass 431 cases, with
8 existing dedicated/manual cases ignored. All 48 UI cases pass after reviewing
and updating two diagnostics for the new retained-permit/witness types; registry
lifetime, unscoped construction and async/nested storage rejection remain intact.
The handles-only `miri_` filter passes all 25 tests under both Stacked and Tree
Borrows on nightly-2026-08-22. The parking_lot_core integer-to-pointer warning
remains visible; these are not strict-provenance certifications. Clippy with
warnings denied, no-default-feature check, i686 handles cross-check, formatting,
panic-boundary and TCB audits, and diff checks pass locally. No Windows runtime,
remote CI, or whole-stack completion claim is made.


## Scoped Handle read authorization and initialized-pointer access

- Scoped BindingTable reads instantiate a shared expression covering domain
  authorization before publication load and dereference, missing-slot rejection,
  identity/Live validation, and witness-retaining reader construction.
- The executable Handle backend borrows real initialized LinearOwner memory via
  `borrow_at` using the loaded pointer. The matching precondition preserves
  provenance; domain address equality alone does not grant memory access.
- FailStop is a distinct modeled outcome for the native abort path. Missing or
  stale records remain ordinary rejection. The accepted reader keeps the same
  borrowed admission token and the exact stored value.
- Remaining adapter premises are explicit: the modeled publication owns a fixed
  record and gate set, and its immutable owner borrow is stronger than the native
  temporal permission that survives retirement queue movement. Native AtomicPtr
  publication, AtomicU8 observation, identity representation, and the queue/
  observation ledger mapping remain open. This is shared read-ordering and
  permission-backed access evidence, not full native temporal read refinement.

Validation: Handle Verus passes 372 obligations; PublishedOwner passes 15 after
adding the pointer-checked borrowing adapter. All 33 Handle mutations fail at the
verification stage across the full script and the focused added load-order case.
The new cases reject domain bypass, publication before authorization, skipped
validation, ignored ID/Live observations, and unrelated allocation pointers.
The load-order mutation initially had a stale formatting anchor; it was repaired
and rerun, not counted as a proof rejection. Native Handle tests pass 118 cases
with one existing Shuttle case ignored. The eight-domain scoped-read regression
passes under both Stacked and Tree Borrows. Handles Clippy with warnings denied,
TCB and panic-boundary audits, formatting and diff checks pass locally. This
increment does not claim a new complete Miri run, Windows run or remote CI.


## Handle observations survive publication-to-retirement ownership transfer

- Replaced the shared reader backend's direct LinearOwner borrow with a
  storage-backed Observation carrying a borrowed gate permit. The stored value
  retains real initialized memory and deallocation rights; observations guard
  that same permission while publication ownership moves to retirement.
- The state machine conserves a unique published-or-retired owner, the exact
  multiset of observations, and its count. Retirement preserves observations;
  reclamation consumes the retired token only with zero observations. Observe
  requires the published token, preventing creation from an arbitrary pointer.
- The shared read returns a reference tied to Observation, independently of the
  publication metadata's borrow. A positive executable witness observes, retires,
  executes the shared read, then ends observation and returns the same retired
  token with the original observation count restored.
- RetiredRecord wraps that linear retirement token. Shared generation registration
  preserves exact payload identity and checks its fixed owner against the queue.
  Recovery requires that allocation's instance and zero-observation count; queue
  transfer alone does not grant reclamation rights.
- Remaining work includes native AtomicPtr load/withdrawal linearization and
  observation creation, an exhaustive observation ledger deriving zero from
  drained matching gate histories, and the native record/arena representation.
  The modeled Live sample is not a proof of native AtomicU8 memory ordering.

Validation: Handle Verus passes 389 obligations. All 40 Handle mutation cases
are rejected at verification across the full run's 33 completed cases and the
focused seven observation/resource cases. An initial state-machine mutation
changed its generated method signature and failed compilation; it was replaced
with a signature-preserving weakening and rerun, not counted as verification
failure. The retirement counterexample also rejects the false claim that moving
publication ownership requires every observation to have ended. TCB audit and
diff checks pass. This increment changes proof code and documentation only;
no new native, Miri, Windows or remote CI result is claimed.


## Handle drain derives empty observation storage before recovery

- Covered allocation initializes the actual heap storage, publication token and
  a private observation ledger. The ledger's map contains every observation,
  including its borrowed gate permit, and its length equals the unique count.
  Observation and completion preserve count, allocation identity and coverage.
- Shared Handle read now accepts an entry borrowed from this conserving ledger;
  publication metadata and retirement payload movement remain independent.
- Both-width sealed-zero stripe histories exclude each mapped permit, deriving
  zero observations before recovering the exact retired allocation. No separate
  caller-supplied zero count is required on this covered recovery path.
- Retirement consumes the publication token and freezes additions. Narrowing
  before shared begin/publication excludes the previously idle generation and
  preserves all entries and counts. Pending-generation drain then derives zero
  while readers in the reopened current generation may remain active.
- Empty-ledger lifetime rebinding preserves its exact count, frozen state and
  coverage. A positive proof observes, ends that observation, consumes admission
  and returns the same empty counter ledger; ended observations do not keep the
  old permit borrowed merely through a type lifetime.
- Native AtomicPtr load-to-observation acquisition, full reader-to-ledger
  correspondence, queue cutoff and stripe/address mapping remain open. These
  are required native representation proofs, not assumptions discharged by
  proving the resource-level ledger empty.

Validation: Handle Verus passes 411 obligations, including both-width history and
pending-generation recovery composition. The complete Handle negative script
passes all 47 cases, including seven new ledger/freeze/coverage/rotation cases;
all fail proof obligations, not parsing or generated-code compilation. TCB audit
and diff checks pass. This increment changed proof code/documentation only;
prior native and Miri evidence was not rerun, and no Windows or remote CI result
is claimed. Full native representation and cross-layer completion remain open.


## Actual vstd atomic publication issues observation resources at load

- AtomicPtr's invariant now holds the unique published resource and observation
  count for one allocation. A non-null load issues the matching Observation in
  the same atomic ghost block; null loads issue none. No loaded-pointer equality
  premise is passed separately by the read caller.
- CAS clearing the exact pointer transfers the publication resource into a
  retirement token. Existing Read values retain their own Observation and can
  still borrow initialized memory. Explicit end returns their count through the
  same atomic invariant. This models normal explicit completion, not native Drop.
- The same read_binding expression used by production now calls this atomic
  backend, validates identity and the sampled Live flag, and returns a reader
  owning the observation. Stale rejection returns its observation before exit.
  A positive executable path retires, copies the existing reader's value, and
  then ends observation.
- The pinned vstd source was checked: these atomics are SeqCst. Native publication
  uses Acquire/Release/AcqRel, so this does not close native weak-memory refinement.
  This backend also fixes one allocation per slot; slot reuse and AtomicU8 Live
  observation are not established. Its atomic count still needs composition with
  the exhaustive ledger and native reader/queue/gate representation.

Validation: Handle Verus passes 418 obligations. All 53 Handle negative cases
are rejected across the complete run's earlier cases and the focused six atomic
cases. The CAS mutation correctly produced a Verus atomic-invariant failure;
the harness initially did not recognize that diagnostic. It now accepts this
specific proof diagnostic only with a nonzero failed-verification summary and
exit code. Five classifier regressions pass, including compiler-error and
missing-summary rejection, and are included in `just verus`. Mutation output is
flushed for live progress. TCB audit and diff checks pass. Native Rust and Miri
were unchanged and not rerun; no Windows or remote CI result is claimed.


## Atomic slot reuse preserves old allocation identity and counters

- Replaced the fixed-allocation atomic backend with a reusable publication slot.
  Prepared publication bundles carry the actual instance, published resource and
  registry receipt. Null-to-pointer CAS publishes that exact bundle; failure
  returns it unchanged. Domain/owner and receipt identities are checked by the
  atomic invariant, alongside exact pointer/memory correspondence.
- Per-allocation Counts has a monotonic membership resource and a private counter
  map. Registration preserves all existing counts; observe/end preserve the exact
  registered-key set and every other allocation's count. Read stores its original
  instance and durable membership receipt, not the currently published instance.
- A resource witness observes an old allocation, registers a new one, ends the
  old observation, and proves the replacement count unchanged. An executable
  atomic witness clears, attempts a distinct allocation's publication, then reads
  the old value and ends its original observation. Competing publication/failure
  does not change that retained memory permission.
- CAS compares addresses. A retired entry is built from the actual returned
  pointer and matching resource, preserving provenance; expected-pointer address
  equality is not promoted into pointer/provenance equality.
- Native still uses Acquire/Release/AcqRel rather than vstd's SeqCst. Its empty
  check and store run under the table writer guard, while this publication backend
  uses CAS; the serialized equivalence remains to be established. Full native
  slot/ID/AtomicU8 representation and composition of the atomic counter registry
  with the exhaustive reader ledger remain open.

Validation: Handle Verus passes 431 obligations. The complete mutation script
passes all 60 cases, including preserved old counters, exact allocation identity,
old-reader completion after replacement registration, duplicate registration,
foreign-domain republication, expected-pointer provenance substitution, and
false publication success. TCB audit and diff checks pass. This increment changes
proof/backend code and documentation, not native Rust; no additional native,
Miri, Windows, or remote CI result is claimed. Slot reuse is now represented in
the SeqCst backend, while the native refinement obligations above remain open.


## Writer authority connects native empty-check/store publication ordering

- Native PublishedBindings::insert and the atomic verifier now instantiate the
  same publish_binding expression: check the slot is empty, fail-stop if occupied,
  then store the new pointer. Native Acquire load and Release store are unchanged;
  the proof backend no longer substitutes null-to-pointer CAS for this operation.
- The tokenized publication authority has matching atomic/writer views. Pointer
  mutation requires both unique resources. A load agrees with the retained writer
  view; store_empty requires that view still be zero and proves that it cannot
  overwrite an existing publication. Atomic readers may update observations in
  between, but cannot change publication without the writer resource.
- Slot construction returns the writer capability. WriterLock stores it behind
  the actual vstd RwLock predicate. The composed wrappers acquire WriteHandle,
  publish/retire using the matching capability, and consume the handle on release.
  Foreign lock authority cannot authorize mutation of another slot.
- Occupied publication is now explicitly PublishStatus::FailStop, distinct from
  successful publication. Its retained proof bundle accounts for the failed
  outcome; it does not represent native continuation after process abort.
- Native parking_lot guard/RegistryState-to-slot identity and Drop/unwind still
  require representation proofs. vstd atomic operations remain SeqCst, so native
  Acquire/Release refinement, AtomicU8 sampling and atomic-to-reader-ledger
  composition remain open. This closes the backend's publication-operation
  mismatch, not all native writer or weak-memory obligations.

Validation: Handle Verus passes 441 obligations. The full negative script rejects
all 64 mutations at verification, including skipped empty-slot checks, diverging
writer/atomic views, and foreign writer-lock authority. Native Handle tests pass
118 cases with one existing Shuttle case ignored. The scoped multi-domain
publication/read regression passes in both Stacked and Tree Borrows. Handles
Clippy with warnings denied, i686 cross-compilation, formatting, TCB and
panic-boundary audits, and diff checks pass. These are local checks, not Windows
execution or remote CI. Native weak-memory and representation work remains open.


## Native publication requires the matching exclusively borrowed guard

- Moved native pointer insertion and removal from PublishedBindings onto a private
  PublicationWriter. Its constructor checks the actual parking_lot guard lock
  address against this BindingTable, then exclusively borrows that guard.
- Both mutation methods require mutable access to the capability. Reservation
  publication, removal commit and retire_all use this path. Native pointer load,
  store and CAS orderings and the shared empty-check/store expression are unchanged.
- The native guard regression rejects a different table and checks lock retention.
  A separate gate compiles the actual copied native workspace successfully, then
  requires specific Rust borrow errors for early guard release and simultaneous
  writer capabilities. These are native compile-fail cases, not SMT obligations.
- The native guard cannot be reused during a live publication capability, closing
  the sharing opportunity left by a shared guard borrow. The correspondence to
  the verifier writer token, complete RegistryState slot invariant, AtomicU8
  sampling, weak-memory semantics and atomic-to-reader-ledger composition remain
  open. This increment is not a complete native synchronization refinement.

Validation: Handle Verus passes 441 obligations; native Handle tests pass 119
cases with the existing dedicated Shuttle case ignored. Both native borrow
mutations are rejected with their expected E0505/E0499 diagnostics after a
successful unmodified baseline. The handles-only miri_ selection passes all 26
cases in both Stacked and Tree Borrows. parking_lot_core emits its existing
integer-to-pointer provenance warning; these runs are not strict-provenance
proofs. Handles Clippy with warnings denied, i686 Windows cross-compilation,
formatting, TCB/panic-boundary audits and diff checks pass. The 64 SMT mutations
were not rerun for this native-only increment; their previous result is recorded
above. No Windows execution or remote CI result is claimed.


## Atomic observation counters authorize the same allocation's resource recovery

- Counts::recover borrows the exact observing token incremented by atomic loads
  and decremented by Read::end. It returns the stored HeapPermission only when
  the allocation is registered and its count is zero. Failure preserves the
  original retirement token rather than consuming ownership or forging memory.
- Slot::recover_resource opens the actual vstd atomic invariant to invoke that
  operation. RetiredRecord transfers its existing linear ticket; no independent
  zero counter is accepted by this recovery path.
- The observation state machine now exposes the positive-count consequence of a
  live observation. Read::reject_recovery_while_observed applies it inside the
  issuing slot's atomic invariant and proves that the same allocation cannot be
  recovered. Slot replacement does not change the reader's allocation identity.
- The composed resource proof follows observe, retire, rejected recovery, end,
  and successful recovery using one counter and the exact original memory.
- The recovery result is tracked ghost state. This is not an executable native
  readiness check, and it does not prove that the native drain implies zero in
  this registry. Atomic-to-exhaustive-permit-ledger composition, native ordering,
  and Box/Drop representation remain open. The existing independent ledger's
  zero-token proof must not be substituted for this registry's unique token.

Validation: Handle Verus passes 449 obligations. The complete Handle negative
suite rejects all 69 mutations with verification failures, including five added
checks for outstanding observations, unregistered count access, foreign
retirement tickets, omitted final reader completion and lost count conservation.
The proof-failure classifier tests and TCB audit pass; no project assumptions or
external bodies were added. Native Rust is unchanged by this increment, so no
new native, Miri, Windows or remote CI result is claimed.


## Owned admission shares provide exhaustive counter coverage

- Added retained_admission storage around the actual admission::permits token.
  Scope::issue creates linear Share tokens; Scope::close requires zero outstanding
  shares before withdrawing the same permit. Each share guards that stored permit
  and therefore excludes zero active count at its actual gate instance.
- Added a consuming conversion from borrowed Observation to an owned memory
  observation token. The conversion makes no independent lifetime guarantee:
  callers must preserve its gate coverage in the registry. Reattaching a borrowed
  permit requires exactly the same gate identity before normal count completion.
- RetainedCounts privately wraps the existing Counts, starts every allocation at
  count zero, and conserves one stored admission share per live indexed observation.
  Completing an observation returns the share for its original Scope, including
  when another Scope belongs to the same gate. The count is not separately modeled
  or replaced during recovery.
- Both word widths derive zero from real sealed stripe histories and matching gate
  ledgers by excluding every stored Share. Recovery consumes the matching retirement
  ticket through the wrapped Counts. A constructed initialized-allocation witness
  also traverses admission, sharing, observation, retirement, completion, original
  permit release and exact heap-permission recovery.
- Atomic SlotGhost still uses Counts and borrowed Observation. The next required
  composition is to install RetainedCounts there and derive end's index/identity
  preconditions from reader tickets under interference, rather than assume them.
  Frozen pending-generation coverage, native CallScope-to-share correspondence and
  Acquire/Release refinement remain open. This is a verified resource adapter, not
  a completed atomic-to-drain or native reclamation proof.

Validation: Handle Verus passes 481 obligations, DrainGate 181, RotatingReadDomain
308 and CacheLease 426. The existing 69 Handle mutations all failed verification
as required. The first added close mutation changed the generated method arity,
so its compiler failure was rejected by the harness rather than counted. After
preserving that signature and checking all anchors are unique, the 10 new share/
coverage mutations were run separately and all failed verification as required.
TCB audit, proof-failure classifier tests and diff checks pass. No native Rust was
changed in this increment; no new native, Miri, Windows or remote CI run is claimed.


## Atomic load/end and drain recovery share the retained-admission registry

- RetainedCounts now records exact gate/scope identities in each linear reader
  ticket. A persistent index registry ties the ticket's issuing index to its
  allocation. End derives live membership from these tokens; it no longer takes
  mutable share-map membership as a caller precondition.
- SlotGhost now owns RetainedCounts. Atomic load issues a Scope share only when
  publication exists, stores it in the registry, and returns the indexed memory
  observation. Atomic end removes that same allocation's observation and returns
  the share for its original Scope. Stale reads complete it before returning;
  accepted reads add exactly one outstanding share. The existing retire/reuse
  executable witnesses now also complete their Scope shares.
- Retirement preserves the queue payload, original pointer provenance, allocation
  instance and persistent counter-registry receipt. The verified writer-lock
  adapter carries this enriched retirement result.
- recover_after_histories_32/64 checks the matching retirement registry and opens
  the actual vstd AtomicPtr invariant. Inside it, exhaustive owned-share coverage
  and matching sealed all-stripe histories derive the exact counter's zero and
  yield the original initialized heap permission. No independent zero-count
  token or assumed mutable ledger state is supplied by the caller.
- This is backend all-stripe composition. Native CallScope/stripe/address and
  queue representation, pending-generation narrowing under ongoing admissions,
  Acquire/Release refinement, AtomicU8 sampling and native Box/Drop remain open.

Validation: Handle Verus passes 495 obligations. The complete Handle negative
suite passes all 86 mutations, including exact ticket gate/scope identity,
allocation/index-registry substitution, foreign retirement registry, omitted
drain conditions and incomplete/wrong-scope reader completion. TCB audit,
proof-failure classifier tests, unique mutation-anchor checks and diff checks
pass. This increment changes the executable proof backend and resource adapters,
not native Rust; no new native, Miri, Windows or remote CI result is claimed.


## Pending-generation recovery preserves active current-generation readers

- The binding resource machine now records persistent retirement history. A
  published resource and that history cannot coexist. RetainedCounts records
  the history; an allocation without it keeps full coverage. Observation still
  requires the real published resource, so a retired allocation cannot gain new
  observations after coverage is narrowed.
- Reader-index coverage contains every stored Share gate. Narrowing requires
  every remaining share to lie inside the selected bound and intersects existing
  coverage with it. Persistent bound tokens prove that later coverage remains a
  subset, even under further narrowing. Index-registry receipts bind those tokens
  to their original allocation.
- Both widths narrow only with no previous pending generation and matching
  GenerationLedgers. The actual stored admission shares exclude observers in the
  idle generation before reuse. Atomic narrow_and_begin issues the bound before
  the production-shared begin/publication expression; swapping the order is a
  negative verification case.
- Atomic recover_after_pending validates that same allocation/index bound inside
  the same AtomicPtr invariant, excludes every remaining share using pending idle,
  and recovers the exact original heap permission. No current-generation idle
  premise is used. An executable witness then reads through a still-live current
  Read from the same publication slot and returns that reader intact.
- GenerationLedgers contains two logical gate identities. Mapping these to the
  native per-generation stripe array, production retirement queue cutoff and
  CallScope remains open, along with native weak-memory and Box/Drop adapters.
  The new result completes the backend pending-generation composition; it does
  not assert completion of the native multi-stripe reclamation path.

Validation: Handle Verus passes 513 obligations. The existing 86 Handle mutations
all fail verification as required. The nine new pending-generation cases also
fail verification, after updating the classifier to recognize the actual Verus
state-machine diagnostic "unable to prove assertion safety condition" together
with a failed verification summary. A dedicated regression still rejects that
message without a failed summary; all six classifier tests pass. The nine cases
were rerun separately after that harness correction. TCB audit and diff checks
pass. Native Rust was not changed, and no new native/Miri/Windows/remote CI result
is claimed. Per-stripe native representation and weak-memory work remain open.


## Exact stripe identity sets support bounded generation reclamation

- StripeLedgers::domain is the finite set of every array position's gate identity.
  member_at proves that any valid position, including the final stripe, belongs.
  The representation no longer needs to collapse an entire generation to one
  logical admission instance for the striped coverage path.
- The coverage update is centralized in narrow_from_exclusion. Both the scalar
  generation proof and arbitrary-stripe proof derive that every retained Share
  lies within the bound before calling the same update.
- narrow_after_idle_stripes requires sealed idle histories covering every gate
  excluded from the allocation domain. A retained share outside the kept set
  identifies an actual idle stripe and contradicts its owned admission token.
- Atomic narrow_stripes_32/64 validates the complete disjoint kept/idle domain
  partition, raw kept-state/ledger mapping, idle histories and retirement evidence
  inside the same AtomicPtr invariant. The kept generation may remain active.
  recover_pending_stripes_32/64 requires the exact pending stripe domain to match
  the persistent bound and derives zero from all those histories. No other/current
  stripe idle premise is required. Executable witnesses recover and then read a
  retained current reader outside the bound.
- This removes the single-gate restriction in the resource backend. The new
  stripe entry points still receive arrays, histories and identity mapping as
  parameters. Their binding to native generation selection/publication-barrier
  control, actual addresses, CallScope and queue cutoff remains open. No native
  production change or complete native multi-stripe refinement is claimed here.

Validation: Handle Verus passes 527 obligations, DrainGate 183, RotatingReadDomain
310 and CacheLease 428. The complete Handle negative suite rejects all 101
mutations at verification, including final-stripe omission, uncovered excluded
gates, undrained excluded stripes, incomplete persistent-bound coverage and wrong
atomic generation partitions. TCB audit, all six proof-failure classifier tests,
unique mutation-anchor checks and diff checks pass. Native Rust was unchanged;
no new native, Miri, Windows or remote CI run is claimed.


## Atomic retirement resources survive queued batch recovery

- queued_retirement::Retirement binds the actual enriched AtomicPtr RetiredRecord
  to its slot and completion owner. Construction requires the same allocation
  counter registry and owner; recovery consumes the original record and bound.
- Registration invokes the production-shared selection/lock/recheck/append loop
  with that payload. Certified ordinary and terminal detachment preserve the
  exact stored records, rather than replacing them with separate LinearOwner
  instances at the queue boundary.
- Both widths prepare every selected queue entry, preserving allocation records,
  queue lengths and the unselected queue. The ordinary barrier precondition holds
  only the OLD/current queue, matching native control; it does not require both
  queues. Registration's held/both_held state is still a protocol representation,
  not proof of native mutex ownership or of the subsequent publication step.
  Preparation and recovery traversals remain verifier-side resource adapters;
  their native driver instantiation is still required.
- recover_batch consumes a certified batch and obtains each exact heap permission
  using matching pending-stripe histories and persistent allocation bounds.
  ordinary_recover composes certificate detachment and recovery directly. The
  RecoveredBatch keeps its completion-owner borrow, payload count and reverse-order
  allocation mapping. Recovery is not destructor completion or debt discharge.
- Remaining adapters include native lock/array/address/history representation,
  the generation publication driver, and actual Box/Drop completion. This change
  strengthens resource composition without claiming the native path is complete.

Validation: Handle Verus passes 550 obligations with zero errors. The complete
Handle negative suite rejects all 107 mutations at verification (exit 0),
including six new registry/owner, final-record preparation, bound retention,
pending-domain and complete-batch recovery cases. The six new cases also passed
an isolated run. TCB audit, all six proof-failure classifier tests, 107 unique
mutation anchors and diff checks pass. Native Rust was unchanged in this
increment; no new native/Miri/Windows/remote CI evidence is claimed.


## Queue guard lifetime crosses generation publication

- Native publish_then_release_barrier now passes a borrow of its owned guard to
  the publication callback. All blocking, polled, idle and Loom uses retain the
  same execution order. Moving guard Drop before publication fails with E0382.
- barrier_ownership acquires a real vstd queue WriteHandle. HeldBarrier borrows
  the handle, queue state and lock and checks their identity/predicate. The
  publication adapter requires matching domain, OLD/current queue index and a
  borrowed matching transition handle, then invokes the shared rotation control.
  Shared publish_release consumes the queue handle only after publication.
- Reversing publish_release now fails the borrow checker with E0505 for both
  handle and queue. That shared mutation moves to the dedicated lifetime gate;
  a separate refinement-body mutation preserves the SMT ordering check. The
  negative-proof classifier still rejects compiler-only failures.
- This is not yet native parking_lot representation refinement. The verified
  queue lock is not connected to queued_retirement payload preparation or native
  generation/stripe history. Those connections and weak-memory/Drop obligations
  remain open; the existing protocol booleans are not silently promoted to native
  mutex ownership evidence.

Validation: RotatingReadDomain Verus passes 316 obligations, Handle 556 and Cache
434, all with zero errors. The complete rotation SMT negative suite rejects 34
mutations (exit 0). The separate Verus lifetime gate rejects early transition and
publication-barrier release; the native gate passes its baseline and rejects all
three native mutations, including early barrier release. Kernel native/Loom tests
pass 67 cases; kernel all-target Clippy, i686 Windows library cross-check, format,
TCB/panic-boundary audits, six proof-failure classifier tests, 34 unique mutation
anchors and diff checks pass. No new Miri, Windows execution or remote CI result
is claimed. The prior SMT-suite attempt stopped because the shared early-release
mutation now fails borrow checking; the final suite and separate lifetime gate
were rerun successfully after separating those checks.


## Locked retirement preparation composes with publication

- QueuePredicate now includes a per-payload invariant, checked at construction
  and restored whenever the verified library write handle is released. The
  generic existing lock constructor uses a trivial payload predicate; the Handle
  constructor requires actual allocation validity, owner and gate-domain facts.
- begin_prepared acquires this queue and derives record facts from its invariant.
  prepare_barrier_queue validates the matching old/current queue and prepares
  every actual retirement. prepare_and_publish issues HeldBarrier borrowing this
  same queue/handle, performs shared publication and preserves record identities,
  queue length, validity and persistent bounds. The caller releases the queue only
  after publication. The returned ghost snapshot describes this completed
  preparation; it does not promise absence of later queue interference.
- Moving preparation after publication violates the old/current and no-pending
  preconditions. Omitting preparation, losing a record, using the completion
  address as rotation identity, or forgetting a payload in the lock invariant
  must also be detected by the new negative cases.
- This closes preparation/publication composition in the resource backend. The
  native parking_lot queue representation, shared registration/detachment on this
  lock backend, live stripe/generation histories, weak memory and Box/Drop remain
  open. No native implementation or full refinement claim is added by this step.

Validation: Handle Verus passes 564 obligations, RotatingReadDomain 317 and Cache
435, all with zero errors. The complete Handle negative suite rejects 112
mutations at verification (exit 0); all five new cases also passed an isolated
run. The complete rotation suite rejects 34 mutations, and both transition/barrier
lifetime checks pass. TCB audit, six proof-failure classifier tests, 112 unique
Handle mutation anchors and diff checks pass. Native Rust was unchanged in this
increment; no new native, Miri, Windows execution or remote CI result is claimed.


## Certified detachment uses the publication queue lock backend

- locked_detachment instantiates production-shared authorization/lock/take control
  with actual vstd queue WriteHandles. Lock index and domain match the certificate
  selection. take_locked preserves the exact protected vector and returns empty
  state to the lock; take_pair extracts both vectors while both handles remain
  held, preserves generation order and only then releases the handles.
- Ordinary and terminal Batch adapters transport the payload predicates and the
  completion-owner borrow. terminal_recover_locked composes terminal detachment
  with all-domain history-based AtomicPtr recovery. Each batch recovery preserves
  its allocation mapping and work count. It performs no destructor/debt work.
- Unlike pending-only recovery, terminal recovery does not need preparation to
  survive a queue unlock/relock interval: every allocation gate must be covered by
  the supplied drained histories. For ordinary recovery that persistent link is
  still open; the payload predicate must not be treated as proof of preparation.
- Native lock/address correspondence, registration using the same lock backend,
  actual generation/stripe history, weak memory and Box/Drop remain open. This
  increment changes verification composition, not native queue implementation.

Validation: Handle Verus passes 583 obligations, RotatingReadDomain 324 and Cache
442, all with zero errors. The complete rotation negative suite rejects all 38
mutations (exit 0). The two new Handle terminal-recovery mutations pass, and the
three existing certified-batch cases were rerun after qualifying the duplicated
payload mutation anchor with its containing function. The previous full Handle
run covered 112 cases; this increment does not claim a fresh full 114-case run.
Both lifetime gates, TCB audit, six proof-failure classifier tests, unique mutation
anchors (Handle 114, rotation 38) and diff checks pass. Native Rust was unchanged;
no new native/Miri/Windows execution or remote CI run is claimed.


## Withdrawal source survives the batch wrapper

- The previous generic locked adapter's output predicate described each returned
  element; that predicate alone would be vacuously true after dropping all output.
  Low-level take_locked already preserved its input vector, but wrapping also
  needs a persistent source relation.
- Withdrawal snapshots the source sequence, owner and generation while the queue
  handle is held. Receipt construction requires the actual vector to equal that
  sequence. Terminal receipts preserve the two source sequences and indices.
- LockedBatch keeps the withdrawal source through bind_withdrawal, and converting
  it to the existing Batch exposes equality with both source and records. Terminal
  recovery consumes these wrappers. Empty-vector substitution and source
  truncation are now explicit negative cases at the wrapping boundary.
- The snapshot describes the protected state at extraction, not a concurrent
  pre-acquisition or post-release observation. This strengthens resource transport;
  it does not establish native address/history correspondence, pending-preparation
  persistence, weak memory or Box/Drop completion.

Validation: Handle Verus passes 588 obligations, RotatingReadDomain 327 and Cache
445, all with zero errors. The complete rotation negative suite rejects all 40
mutations (exit 0). The two new Handle wrapper mutations and two affected terminal
recovery mutations pass. This is targeted Handle validation, not a fresh full
116-case run. Both lifetime checks, TCB audit, six proof-failure classifier tests,
unique mutation anchors (Handle 116, rotation 40) and diff checks pass. Native Rust
was unchanged; no new native/Miri/Windows execution or remote CI run is claimed.


## Preparation authority survives queue unlock and reacquisition

- queue_preparation tracks one queue phase with linear ready/prepared tokens and
  a matching protected state token. freeze consumes ready and issues the exact
  prepared bound; reset consumes prepared before ready can reappear. A live ticket
  cannot be copied or reused for another preparation, even with the same index.
- QueuePredicate identifies this phase instance and requires every payload to
  satisfy prepared_inv whenever its protected phase is prepared. Queue creation
  returns ready; Handle's predicate binds prepared_inv to each actual Retirement
  coverage bound. Preparation verifies records before freezing and publication.
- take_prepared reacquires the same lock, proves phase agreement from the ticket,
  and derives preparation for every stored payload. It extracts the exact source,
  resets only the empty queue, releases the handle, and returns ready. Shared
  certificate authorization rejects foreign domains while returning the unchanged
  prepared ticket; it does not silently consume retry authority.
- recover_prepared_locked connects this result to matching pending-stripe drain
  histories and exact AtomicPtr heap recovery. Ready is returned to its caller
  after recovery completes. This closes unlock/relock preparation persistence in
  the resource backend; it is not destructor completion or debt discharge.
- Registration on this queue-lock backend, native mutex/CallScope/address mapping,
  generation reuse/history, weak memory and Box/Drop still need refinement. No
  native code changed and no project assumption was added for those boundaries.

Validation: Handle Verus passes 605 obligations, RotatingReadDomain 342 and Cache
460, all with zero errors. The complete rotation negative suite rejects all 45
mutations at verification (exit 0). Its five phase/foreign-ticket/reset cases also
passed an isolated run. Six affected/new Handle preparation/domain/coverage cases
pass; no fresh full 118-case Handle run is claimed. An initial phase-retention
mutation changed a generated function signature and stopped at type checking; it
was replaced by a signature-preserving wrong-phase mutation, then the complete
suite was rerun. Both lifetime checks, TCB audit, six proof-failure classifier
tests, unique anchors (Handle 118, rotation 45) and diff checks pass. Native Rust
was unchanged; no new native/Miri/Windows execution or remote CI result is claimed.


## Production queue movement shares executable expressions

- Handle and Cache enqueue plus ordinary/terminal extraction now use the shared
  retirement_queue.rs append_retired/take_retired macros. Their lock scopes,
  generation checks, debt/queued updates and memory order remain in place.
- Registration's queue append and the actual-handle ordinary/terminal/prepared
  extraction backends instantiate that same source. append_ready additionally
  composes a matching handle and ready authority with the shared append expression
  and preserves the exact queue sequence and lock predicate.
- take_retired uses Default plus mem::swap, preserving the native SmallVec storage
  and the previous mem::take behavior. Initial Vec-specific construction failed
  native compilation and was corrected to Default; no storage conversion was
  introduced. Verus proves the shared expression using Vec contracts. SmallVec's
  layout/implementation is not thereby mechanically refined into Vec.
- A live sampled-current/ready-authority connection and registration retry on the
  actual queue-lock backend remain open, as do native address/history, weak memory
  and Box/Drop. This increment strengthens production code reuse rather than
  claiming those remaining boundaries are closed.

Validation: Handle Verus passes 606 obligations, RotatingReadDomain 343 and Cache
461, all with zero errors. The complete rotation negative suite rejects 47 cases,
including both direct production-source mutations; those two also passed an
isolated run. Cache ownership (28) and pin (3) negative suites pass, as do Verus
transition/barrier lifetime checks and all three native publication borrow checks.
No fresh full Handle 118-case negative run is claimed. The handles,cache native
library suite passes 414 tests with 8 existing ignores; targeted miri_ tests pass
29 each under Stacked Borrows and Tree Borrows on nightly-2026-08-22. Both Miri runs
retain the existing parking_lot_core integer-to-pointer provenance warning; these
are targeted results, not complete provenance certification. Feature-matched
Clippy with denied warnings, i686 library cross-check, formatting, TCB/panic audits,
six classifier tests, all affected unique anchors and diff checks pass. No Windows
execution, remote CI or native performance benchmark is claimed.


## Atomic current selection derives registration readiness

- current_atomic uses a real vstd AtomicBool whose ghost invariant owns the current
  queue's ready authority or, while publication is reserved, its phase-state token.
  The actual acquired queue owns that same phase-state identity. Uniqueness rules
  out simultaneous publisher custody, so a matching atomic recheck derives open
  phase without assuming a current-implies-ready relation from the caller.
- atomic_registration instantiates the production register_retired retry expression
  with actual atomic selection/recheck and queue WriteHandles. A stale queue is
  released before retry; successful append preserves the exact protected source
  sequence plus the input payload. Handle's register_atomic accepts its enriched
  Retirement and returns this exact insertion receipt.
- reserve consumes the held queue state, verifies the prepared payload predicate,
  freezes the phase and transfers its state token into the atomic invariant.
  ReservedQueue retains the exact payloads, bound and linear publication authority.
  publish_reserved consumes that reservation, installs the next queue's ready
  token at the atomic store, and returns the old queue state and prepared token.
  Both sides of the custody invariant therefore have executable implementations.
- This closes current/recheck/readiness composition in the SeqCst resource backend.
  It does not connect these methods to native AtomicU8 ordering or the actual
  seal/reopen/drain/reuse driver. Native parking_lot, SmallVec, stripe/address
  representation and Box/Drop remain open. Retry and acquisition prove safety
  on return, not fairness or termination. No native source changed in this step.

Validation: RotatingReadDomain verifies 360 obligations, Handle 624 and Cache 478,
all with zero errors. The registration early-release mutation is now rejected by
Rust E0382 because it consumes the actual guard; it was moved to the dedicated
borrow gate rather than accepted as a failed SMT proof. The separate logical
held-authority mutation fails verification. All three Verus borrow checks pass,
as do TCB audit (zero assumes and unapproved trusted bodies), the six proof-failure
classifier tests and diff checks. The complete rotation suite rejects all 52
mutations at verification; the five new current-atomic cases also passed an
isolated run. No new native/Miri/Windows or remote CI result,
and no fresh full Handle negative run, is claimed for this proof-only increment.


## Handle queue publication uses atomic current and shared barrier release

- Current's raw publish_reserved helper is private. Its public publish_and_release
  consumes the actual queue WriteHandle and runs the production publish_release
  expression, performing the atomic store before returning the prepared state to
  the lock. It returns the exact published source snapshot and prepared token.
- Handle's prepare_publish_atomic, instantiated for 32 and 64 bits, acquires the
  actual queue, rechecks Current, derives open phase, prepares every retirement's
  persistent stripe bound, reserves the queue and invokes publication/release.
  This uses the same Current and queue resource types as register_atomic. It no
  longer needs a separately supplied abstract Rotation.current for this queue
  half of publication. Both stale-selection paths release the acquired handle
  and return the original next-ready token.
- This does not yet compose admission sealing, reopening or the drain driver with
  atomic current. In particular, the kept/idle ledger partition and histories
  still need native generation/address correspondence. SeqCst versus native weak
  memory, SmallVec/parking_lot representation and Box/Drop remain open.

Validation: RotatingReadDomain verifies 361 obligations, Handle 627 and Cache 479,
with zero errors. The five current-atomic rotation mutations pass an isolated
rerun. Three new Handle mutations (missing current recheck, missing preparation,
foreign next-ready authority) fail at verification. The shared early-release
borrow gate additionally requires the current_atomic E0382 diagnostic, checking
that the atomic publication path itself retains its handle. All three borrow
checks and the zero-assume/approved-body audit pass. Unique mutation anchors are
121 for Handle and 52 for rotation. No fresh full Handle/rotation negative suite
or native/Miri/Windows execution is claimed for this proof-only increment.


## Actual gate CAS owns admission through AtomicPtr borrowing

- DrainGate atomic_counter instantiates actual vstd AtomicU32/AtomicU64. The
  invariant equates masked machine count with the admission instance's active
  token; acquire/release call the existing production-shared transition kernel.
  Acquire issues a linear permit only on successful CAS. Failed CAS attempts
  preserve resources; release consumes the matching permit only when it updates
  the actual atomic. Overflow remains explicitly classified as FailStop.
- acquire_scope stores that actual permit in retained Scope; release_scope needs
  zero outstanding shares. observe_with_share proves an actual load cannot see
  zero active count while a matching observation share is live.
- Handle admitted_read (32/64-bit) composes the actual gate CAS with AtomicPtr
  load, heap-backed borrow, observation return and final gate release. Its opaque
  owner contains both Read and Scope. Ending returns the observation before
  releasing the counter, and an empty slot returns its unused admission.
- This closes an actual-atomic admission-to-read resource connection, not the
  whole native lifecycle. Counter selection/slot-domain membership are still
  caller premises. Native stripe/address mapping, seal/reopen/current composition,
  waiter mutex/notification, weak memory and Box/Drop remain open. This proof
  backend classifies fail-stop; it does not implement the native abort adapter.

Validation: DrainGate 205, RotatingReadDomain 383, Handle 655 and Cache 501 verify
with zero errors. Five new counter mutations and two admitted-read mutations fail
at verification, including permit issuance/consumption on failed CAS, foreign
permit release, closing with live observations and reading outside the gate
set. All 32 DrainGate, 123 Handle and 52 rotation mutation anchors are unique;
these counts do not claim fresh complete negative-suite executions. TCB audit,
six failure-classifier tests and diff checks pass. No new native/Miri/Windows
execution or remote CI result is claimed for this proof-only change.


## Atomic sealed bit agrees with exclusive lifecycle control

- Atomic counter state now owns both active-count and lifecycle tokens. The
  invariant ties the lifecycle token to the real sealed bit; a separate unique
  control token agrees with it. Counter construction returns that authority.
- seal requires matching mutable control and updates both views at actual
  fetch_or. reopen calls the production-shared sealed-zero transition and changes
  control only at successful CAS. Rejected reopening preserves control unchanged;
  acquire/release preserve the sealed bit and its resource correspondence.
- observe_control derives the actual loaded bit from borrowed matching control.
  acquire_controlled and ordinary acquire instantiate the same acquire loop, with
  the former proving sealed authority implies Rejected. Retried CAS attempts and
  permit ownership stay within that same implementation.
- Native transition mutex/current publication ownership of this control token is
  not yet connected. Stable drained certificates, full generation reuse, waiter
  notification and weak memory remain open; these local lifecycle contracts do
  not close the entire lifecycle row.

Validation: DrainGate 226, RotatingReadDomain 404, Handle 676 and Cache 522 verify
with zero errors. All ten affected atomic-counter mutations reject at verification
(nine together plus the new controlled-acquire case separately), including wrong
controller/bit agreement and reopening with admission logic. Unique anchors are
37 for DrainGate, 123 for Handle and 52 for rotation. TCB audit, six classifier
tests and diff checks pass. No fresh whole negative-suite, native/Miri/Windows
execution or remote CI result is claimed for this proof-only increment.


## Lock-owned atomic lifecycle follows production rotation order

- atomic_rotation stores both counters' lifecycle controls in an actual vstd
  transition lock. Its predicate binds authority identities and requires pending
  work to name a sealed generation. begin needs the matching actual transition
  handle, both mapped counters and the reserved old queue.
- begin_rotation, publish_reopen and publish_release instantiate the same nested
  order as production: old seal, pending assignment, actual Current publication,
  next reopen, queue release. Publication requires both gates sealed; a matching
  reservation identifies the exact old queue. The next counter's reopen uses the
  previously verified actual CAS and shared sealed-zero kernel. A failed reopen
  enters a non-returning branch corresponding to native fail-stop.
- The raw Current store helper is now module-internal for this driver. The older
  queue-only public adapter remains used by Handle and must still be migrated;
  this increment does not claim that every publication entry already requires
  counter authority. Native mutex/stripe/address mapping, persistent drain
  certification, callback/reuse, weak memory and Box/Drop remain open.

Validation: RotatingReadDomain 418, Handle 690 and Cache 536 verify with zero
errors. All three lifetime gates pass; the early-release gate now additionally
requires atomic_rotation's moved-queue_handle diagnostic. TCB audit and unique
anchors (Handle 123, rotation 56), six classifier tests and diff checks pass.
The complete rotation negative suite rejects all 56 mutations at verification.
No fresh native/Miri/Windows execution, remote CI or full
Handle negative-suite result is claimed for this proof-only increment.


## Handle publication requires the combined atomic rotation driver

- prepare_publish_atomic now borrows the actual transition state/handle before
  acquiring its queue and requires the mapped counter authorities plus a sealed
  replacement. After recheck and record preparation it invokes atomic_rotation
  begin, so old seal, pending assignment, publication, next reopen and barrier
  release are composed on this Handle path.
- Success proves pending old, old sealed and next open along with the prepared
  snapshot/ticket. Both stale-selection returns preserve the transition state
  exactly and return next-ready authority unchanged.
- Removed Current.publish_and_release. The sole source caller of the internal
  publish_reserved method is now atomic_rotation's checked publication helper;
  there is no remaining queue-only public Current store path. Native mapping and
  persistent drain/callback/all-stripe composition remain open.

Validation: Handle 689, RotatingReadDomain 417 and Cache 535 verify with zero
errors (the deleted queue-only adapter removes one obligation from each import).
All six affected Handle publication mutations and three lifetime checks pass;
TCB audit reports zero assumes/unapproved bodies. All 12 affected rotation
mutations pass (three shared order, five current custody, four driver authority
cases). Unique anchors (Handle 126, rotation 56), six classifier tests and diff
checks pass. No fresh whole Handle negative suite
or native/Miri/Windows/remote CI execution is claimed for this proof-only change.


## Actual zero observation issues stable drain ownership

- try_drain requires matching sealed control and observes the actual counter.
  A zero observation stores the exact active token and controller in a verified
  storage state machine and returns opaque DrainLease; a busy observation returns
  control unchanged. The atomic invariant requires the frozen word sealed and
  zero, retaining the same admission and lifecycle identities.
- Existing acquire/release/seal/reopen paths are verified across this frozen
  phase. Successful acquire cannot observe a frozen sealed word. A matching live
  permit/share contradicts its stored zero token, and mutable control contradicts
  uniqueness of the stored controller. These are resource contradictions, not
  additional assumptions about absent readers.
- DrainLease lends the zero token and excludes a matching admission share. restore
  consumes the ticket, returns the active token to the atomic invariant and gives
  back sealed control. The borrow gate rejects consuming that lease while a
  tracked zero-resource borrow remains in use. The initial probe used only a spec
  assertion, which does not retain the borrow; it was corrected to an actual
  tracked-argument use after restore before recording a passing negative check.
- This is a single-counter drain capability. Pending transition state, full-stripe
  identity/aggregation and actual Handle/Cache heap recovery still need to consume
  it in place of caller-supplied drain histories. Native wait/notify, representation,
  weak memory and Box/Drop remain open.

Validation: DrainGate 240, RotatingReadDomain 431, Handle 703 and Cache 549 verify
with zero errors. All 15 atomic-counter negative cases reject at verification,
including nonzero/unsealed drain, foreign restoration and foreign-share exclusion.
All four dedicated borrow gates pass. TCB audit, six classifier tests, unique
anchors (DrainGate 42, Handle 126, rotation 56) and diff checks pass. No fresh
complete negative-suite, native/Miri/Windows execution or remote CI result is
claimed for this proof-only increment.


## Owned atomic drain sets recover the exact retired allocation

- DrainSet owns DrainLeases by gate identity. Insert/remove preserve exact lease
  values and frame every unaffected entry, enabling later restoration without
  losing the original counter association. Coverage means every required gate
  has a corresponding actual drain resource, not a sampled zero integer.
- RetainedCounts excludes each potential stored Share using its matching lease
  and derives the allocation's observation count is zero. Slot's full-domain and
  bounded recovery methods open the same actual AtomicPtr registry used for
  load/end and return precisely the retired entry's initialized heap permission.
- Persistent-bound narrowing can now exclude all idle/outside gates using actual
  leases too. The bounded recovery path demonstrably leaves a current reader in
  another gate usable. Queued Retirement adapters expose full and prepared lease
  recovery while preserving the exact allocation.
- The new methods do not accept fabricated drain histories. Their integration
  into Handle's existing prepare/publish/detach traversal is still pending; that
  traversal currently retains its prior history inputs. Executable all-stripe
  collection/rollback, Cache migration, transition/callback reuse and native
  address/ordering/Box/Drop correspondence remain open.

Validation: DrainGate 244, RotatingReadDomain 435, Handle 716 and Cache 553 verify
with zero errors. All six new lease-recovery mutations reject at verification,
covering missing gate coverage, registry mismatch, bound coverage and collection
identity/resource loss. TCB audit, six classifier tests, unique anchors (Handle 132,
rotation 56) and diff checks pass. No fresh complete negative-suite,
native/Miri/Windows execution or remote CI result is claimed for this increment.


## Actual stripe collection preserves controllers across partial drain

- atomic_stripes Collection owns a vector of completion flags, the remaining
  sealed controller map and the actual DrainSet. Its invariant ties each flag to
  the exact counter's lease or matching controller, with distinct gate identities.
- poll_all uses production observe_stripes. Busy counters retain their controller;
  completed leases survive subsequent polls. Its result is exactly all stripes
  ready. drains lends the owned set only after that condition, restore_all returns
  mapped leases to their actual counters, and into_controls returns the sealed
  controllers for subsequent reopening.
- Directly passing an idle DrainSet and the same next-generation controller to
  preparation/publication would duplicate ownership. The collector now provides
  the needed borrow/restore boundary; Handle migration must use it before reopen.
  Initial counter identity/controller mapping and native vector/array refinement,
  transition/callback integration, Cache migration and weak memory remain open.
- The mutation harness now verifies its untouched copied tree before applying
  any mutation, requiring both success and a nonzero verified/zero-errors summary.
  The initial collection negative run occurred before its baseline passed and is
  discarded as evidence. The affected cases were rerun after a clean baseline with
  this mandatory gate. Two unit tests prevent pre-existing proof failure or an
  exit-zero/no-verification result from being accepted as a valid baseline.

Validation: DrainGate 264, RotatingReadDomain 455, Handle 736 and Cache 573 verify
with zero errors. Five collection/restoration and three shared stripe-scan
mutations pass after baseline validation. TCB audit, all eight failure/baseline
classifier tests, unique anchors (DrainGate 47, Handle 132, rotation 56) and diff
checks pass. No fresh complete negative
suite or native/Miri/Windows/remote CI execution is claimed for this increment.


## Collected leases prepare records before controller restoration

- Handle prepare_collected_records composes actual all-stripe collection with
  per-record persistent-bound narrowing. It borrows the complete DrainSet only
  while preparing records, then restores each lease and returns matching sealed
  controllers. Every excluded gate must be present in the counter vector.
- A busy stripe returns the partial Collection without changing any record.
  Success prepares every record for the retained gate set while preserving the
  exact allocation identity, completion owner, gate domain and sequence length.
- This closes the collection-to-record-preparation handoff, not the whole driver.
  prepare_publish_atomic still takes histories; its two scalar controllers are
  not yet connected to the returned arbitrary-stripe controller map. Queue-lock
  composition, Cache migration, native synchronization/order/address/Box/Drop
  refinement and the complete reclamation path remain open.

Validation: Handle verifies 743 obligations with zero errors. The untouched
baseline also passes before all five new negative checks. Those checks reject skipping the final record, losing
its persistent bound, bypassing actual collection, returning controllers before
restoration and omitting an excluded counter. All eight harness unit tests and
137 unique Handle mutation anchors pass. No full negative-suite rerun or new
native/Miri/Windows/remote CI result is claimed for this proof-only increment.


## Collected preparation uses the protected queue payload

- prepare_collected_queue borrows the matching library WriteHandle and applies
  collection-based preparation to that QueueState's records. Payload validity,
  completion ownership and gate-domain premises follow from the queue lock
  invariant instead of independent vector premises supplied by the caller.
- It requires an open preparation phase, preserves phase/instance/queue identity,
  and restores the lock invariant for both completed preparation and partial-drain
  retry. The caller retains the actual handle for reservation and publication.
- This is the protected preparation entry only. Existing prepare_publish_atomic
  still needs migration from histories and scalar controls to this arbitrary
  stripe controller handoff. Acquisition/recheck/reservation/publication composition,
  Cache and native synchronization/representation boundaries remain incomplete.

Validation: Handle 745 verified, zero errors. Three new mutations reject a foreign
lock handle, modification of a frozen preparation bound and loss of the protected
payload invariant, after a clean untouched baseline. All 140 Handle mutation
anchors are unique; TCB audit and diff checks pass. No new full-suite, native,
Miri, Windows or remote CI result is claimed.


## Collected preparation hands exact records to atomic reservation

- prepare_reserve_collected holds a borrowed matching queue WriteHandle across
  Current.recheck, actual collection-based preparation and Current.reserve. A
  successful reservation retains every original record identity and the prepared
  coverage bound, together with the restored sealed controller map.
- An initial stale selection or busy drain returns the queue and partial
  Collection without changing records. If selection changes after preparation,
  reservation rejection returns the owned queue and restored controllers. The
  result distinguishes partial collection from restored controller ownership.
- This composes the protected preparation with real atomic reservation. It does
  not yet acquire the queue itself or transfer an arbitrary stripe controller map
  into the existing scalar transition/publication/reopen driver. Cache migration
  and native weak-memory, synchronization and raw-pointer correspondence remain
  open; the full goal is not achieved.

Validation: Handle 747 verified, zero errors. Three new mutations reject skipping
recheck, reserving a different bound and proceeding after a busy drain, with a
clean baseline first. All 143 Handle mutation anchors are unique. TCB audit and
diff checks pass. No fresh full negative suite, native/Miri/Windows or remote CI
execution is claimed for this proof-only increment.


## Restored stripe controllers drive the shared ordered reopen scan

- Production StripedDrainGate, Loom and Verus now instantiate reopen_stripes.
  Native locking and the prior sealed/idle precheck remain in place. The shared
  scan visits stripes in order and stops at the first rejected reopen.
- atomic_stripes::reopen_all consumes mutable access to the returned controller
  map and invokes actual counter CAS reopening. Its postcondition preserves map
  domain and per-counter authority, describes the opened prefix and sealed suffix,
  and frames entries outside the counter vector. A loop exit obligation prevents
  early termination without a real rejected attempt.
- Transition ownership/publication composition is still open: this is the actual
  map-to-reopen adapter, not proof that the complete native rotation driver uses
  the verified resource handoff. Native weak memory, Cache integration and the
  full reclamation path remain incomplete.

Validation: DrainGate 270, RotatingReadDomain 461, Handle 753 and Cache 579 verify
with zero errors. Four new mutations reject skipped final reopen, ignored
rejection, foreign controller identity and lost controller ownership. Three
existing stripe-scan mutations pass again after disambiguating their anchors.
All checks require an untouched successful baseline first. DrainGate/Handle have
51/143 unique mutation anchors. The seven DrainGate tests (including Loom) pass;
formatting, kernel all-target Clippy with warnings denied, TCB audit and diff
checks pass. No native Miri/Windows or remote CI result is claimed.


## Stripe collection owns exactly its mapped resource domain

- Strengthened controls_match and Collection::new to require precisely the finite
  counter-index domain. Collection's invariant now forbids unrelated controllers
  and drain leases, in addition to the per-row ownership correspondence.
- Polling and restoration preserve these domain constraints. The complete drain
  borrow equals the counter gate set. into_controls proves the retained DrainSet
  is empty and returns exactly the mapped sealed controllers.
- This closes an ownership-specification weakness before transition integration:
  a superset map could previously carry unrelated resources, and inv alone did not
  rule out an extra lease being discarded at extraction. It does not supply the
  missing native/transition constructor or complete publication/Cache integration.

Validation: DrainGate 274, RotatingReadDomain 465, Handle 757 and Cache 583 verify
with zero errors. Three new negative checks reject extra initial controllers and
invariants permitting unrelated controllers or leases, after a clean baseline.
All 54 DrainGate and 143 Handle anchors are unique. TCB audit and diff checks pass.
This proof-only increment does not claim a new full negative suite or native,
Miri, Windows or remote CI run.


## Exact controller maps seal every actual stripe

- Production StripedDrainGate, Loom and Verus now share seal_all_stripes for
  unconditional sealing. The separate idle-only seal/rollback path is unchanged.
- atomic_stripes::seal_all accepts an exact controller map with arbitrary flags,
  invokes each actual atomic seal, preserves the counter/controller identities and
  returns controls_match with every stripe sealed. This supplies the group-level
  operation needed before pending publication and later drain collection.
- Generation-map ownership under the transition lock and composition with the
  existing reservation/publication/reopen sequence remain open. This adapter does
  not by itself establish the full native rotation or reclamation path.

Validation: DrainGate 278, RotatingReadDomain 469, Handle 761 and Cache 587 verify
with zero errors. Three mutations reject skipping the final seal, omitting actual
seal actions and accepting an unrelated controller map, after clean baselines.
DrainGate/Handle anchors are unique (57/143). Seven native DrainGate tests including
Loom, kernel all-target Clippy with warnings denied, format, TCB audit and diff
checks pass. No new full negative suite, Miri/Windows or remote CI run is claimed.


## Arbitrary-stripe generation maps own actual publication transitions

- Added striped_rotation's actual RwLock State with exact controller maps for
  both generation vectors. Its invariant preserves their identities, distinct
  gates across generations, and sealed authority for the pending generation.
- begin now composes the production-shared seal/pending/publish/reopen/queue-release
  order with actual all-stripe atomics and Current's owned atomic publication.
  The matching transition handle and queue handle are required; the reserved bound
  equals the old generation's gate set. Normal completion leaves old sealed and
  pending, new open, and returns the exact prepared queue authority.
- Partial reopen retains fail-stop nonreturn semantics. The first negative run
  showed pending-sealed invariant deletion was not detected by the initial helper
  contract; reopen_next now explicitly guarantees the old pending group remains
  sealed, and the strengthened case rejects.
- This is the arbitrary-stripe publication driver, not its complete migration.
  Existing Handle prepare_publish_atomic still uses the scalar driver. Its
  collection handoff must take/restore the next map under the new transition
  ownership before invoking begin. Native constructor/lock/weak-memory mappings,
  Cache, callbacks and destruction remain incomplete.

Validation: RotatingReadDomain 483, Handle 775 and Cache 601 verify with zero
errors. Six new mutation checks reject a foreign transition handle, missing pending
sealed invariant, wrong counter vector, wrong reserved generation, wrong gate-set
bound and normal return after partial reopen. Each group starts from a successful
untouched baseline; 62 rotation anchors are unique. TCB audit and diff checks pass.
No fresh full negative suite, native/Miri/Windows or remote CI run is claimed.


## Handle collection transfers and restores transition-owned generation maps

- CollectionHandoff consumes striped State, moves the next generation's exact
  map into Collection, retains the old map, and borrows the actual transition
  WriteHandle. State cannot be supplied to publication until restore consumes a
  matching sealed next map and reconstructs the transition invariant.
- Handle prepare_publish_striped now acquires its queue, creates the handoff,
  composes actual recheck/collection/preparation/reservation, restores State and
  calls the arbitrary-stripe publication driver. Success leaves old pending/sealed,
  new open and the prepared ticket bound equal to the old gate set.
- Busy or initial stale selection returns the handoff, partial Collection and
  unchanged next-ready authority after queue release. Reservation rejection after
  preparation returns restored State and next-ready authority. No duplicated
  controller ownership or fabricated histories are used in this entry.
- The original scalar/history path remains alongside the new path. Constructor
  premises, explicit retry/cancel integration, pending detachment/recovery, Cache,
  native lock/weak-memory/address correspondence and destruction remain open.

Validation: RotatingReadDomain 487, Handle 781 and Cache 605 verify with zero
errors. Four handoff mutations and two Handle integration mutations reject after
successful baselines. All five lifetime checks pass, including the new E0505 probe
for moving the transition handle while the collection handoff is later restored.
Rotation/Handle anchors are unique (66/145). TCB audit and diff checks pass. No
fresh full negative suite, native/Miri/Windows or remote CI run is claimed.


## Busy collection has a shared retry path and resource-preserving cancellation

- Extracted resume_publish_striped as the common owned handoff/partial collection
  path. prepare_publish_striped delegates to it after acquisition and map transfer.
  A caller resuming a busy result reacquires the matching queue and supplies its
  actual handle; generation identity and transition ownership are checked.
- CollectionHandoff::cancel restores every acquired lease, extracts the exact
  controller map, and reconstructs State with pending None, next sealed and the
  old map unchanged. It does not fabricate idle evidence or publish a generation.
- These close the retry/cancel API gap for the new preparation/publication path.
  Pending detachment/recovery, constructor premises, native synchronization/order/
  representation, callbacks, Cache and destruction remain incomplete.

Validation: RotatingReadDomain 489, Handle 785 and Cache 607 verify with zero
errors. Four mutations reject cancellation without restoration, wrong cancellation
counter vector, wrong retry generation and unrelated queue handle, after clean
baselines. Rotation/Handle anchors are unique (68/147). TCB audit and diff checks
pass. No fresh full negative suite or native/Miri/Windows/remote CI run is claimed.


## Pending generation leases recover the protected retirement allocations

- Added PendingHandoff: it consumes matching pending State, transfers the pending
  map into actual stripe collection and retains the live generation map. Restore
  and cancellation preserve pending Some; neither claims callback completion.
- recover_pending_striped polls actual pending counters and preserves the handoff,
  partial collection and prepared ticket on busy return. Once complete it borrows
  the exact DrainSet, acquires the prepared queue, performs shared take/reset/release
  and recovers all records against the same AtomicPtr counter registry.
- The recovered vector contains exactly the withdrawal's allocation permissions in
  pop order. Leases are restored only after the borrowed recovery ends; State keeps
  the live-generation controller map unchanged and still marks the old pending.
- This connects actual pending collection to protected detachment and heap recovery.
  Callback-completion evidence before clearing pending, constructor premises,
  native synchronization/order/address correspondence, Cache and destruction remain
  incomplete. Heap permission recovery is not native Box/Drop completion.

Validation: RotatingReadDomain 495, Handle 797 and Cache 613 verify with zero
errors. Six negative checks reject wrong pending state, premature pending clearing,
foreign restored controllers, bypassed drain, mismatched prepared coverage and
missing final allocation, after clean baselines. Rotation/Handle have 71/150 unique
anchors. TCB audit and diff checks pass. No fresh full negative suite or native,
Miri, Windows or remote CI run is claimed.


## Shared callback return clears pending with complete drain and queue readiness

- PendingHandoff::finish requires complete actual stripe collection and ready
  authority tied to the pending queue in Current. It restores all leases and
  clears pending while preserving both generation controller mappings.
- recover_pending_striped now instantiates production finish_rotation. The exact
  recovered batch, ready authority and withdrawal snapshot are established before
  the finish effect. Busy return is unchanged; ordinary restore/cancel still keep
  pending Some. Successful callback return now produces pending None.
- The first reordered-callback probe stopped at an unrelated type-inference error;
  that result was discarded. Explicit callback-result typing lets the probe reach
  the intended borrow check: consuming collection in finish before recovery causes
  E0382 when the callback later tries to borrow it.
- Pending clearing is retirement callback completion, not Box/Drop completion.
  Constructor premises, native state/lock/weak-memory/address correspondence,
  Cache integration and destruction remain incomplete.

Validation: RotatingReadDomain 497, Handle 799 and Cache 615 verify with zero
errors. Two new SMT mutations reject omitted pending clear and wrong Current queue
readiness. All six lifetime checks pass, including reordered callback/clear, each
suite after its untouched baseline. Eight failure-classifier unit tests pass;
rotation/Handle anchors are unique (72/151). TCB audit and diff checks pass. No
fresh full negative suite or native/Miri/Windows/remote CI run is claimed.


## Cache borrows actual atomic admission and derives recovery from drain leases

- Added Cache atomic_admission for both widths: actual CAS acquisition supplies an
  owned matching permit wrapper, observation borrows that permit into the existing
  ledger, and release returns it through the matching actual counter. Rejected and
  fail-stop admission return no capability.
- DrainLease and DrainSet now exclude a matching borrowed admission permit. Cache
  observations use that authority to derive zero counts and narrow frozen coverage.
  recover_after_drain recovers the exact retired allocation from that zero result,
  matching zero-pin/allocation resources and the final-pin retirement ticket.
- Native Cache pin atomics/index/ledger representation, full preparation/queue/
  reclamation driver migration, weak memory and destruction remain incomplete.
  These are connected actual resource adapters, not a native Cache completion claim.

Validation: DrainGate 280, RotatingReadDomain 499, Handle 801 and Cache 629 verify
with zero errors. Six new Cache mutations reject foreign drain/admission identity,
missing excluded/observed gate coverage and unrelated node domains. All seven
lifetime gates pass, including E0505 when release precedes the last observation's
end. Baselines are verified first; all 34 Cache ownership anchors are unique.
TCB audit and diff checks pass. No fresh full negative suite or native/Miri/Windows/
remote CI run is claimed for this proof-only increment.


## Cache atomic pin updates and recovery share one count invariant

- Added 32/64-bit atomic_pins with count/retiring tokens inside its actual atomic
  invariant. Successful observed and anchored CAS issue the matching linear pin;
  failed CAS retries retain resources. A ghost sample ties the returned acquisition
  classification to the production-shared kernel, including Zero versus Overflow.
- fetch_sub consumes the owned pin, derives non-underflow from conservation, and
  creates a final retirement ticket only at the last pin. release_covered turns it
  into the exact RetiredNode and freezes its observation ledger.
- Scoped Cache observations can now acquire a lease through that atomic adapter.
  Recovery uses the same atomic count/retiring tokens and final ticket together
  with actual drain-derived zero observations to withdraw the exact heap permission.
- Native fetch_update correspondence, pin memory order/fence/release sequence,
  node/index/ledger representation, full queue-driver integration and Box/Drop
  remain incomplete. No full native Cache refinement claim is made.

Validation: Cache 651 verified, zero errors. Eight negative checks reject pin
issuance on failed observed/anchored CAS, lost count conservation, omitted nonfinal
release, foreign pin release, missing final freeze, missing drain coverage and
Overflow/Zero confusion. The baseline verifies before mutations; all 42 Cache
ownership anchors are unique. TCB audit and diff checks pass. No fresh full
negative suite, native/Miri/Windows or remote CI run is claimed.


## Cache allocation resources initialize the same pin atomic

The 32/64-bit `atomic_pins::initialize` entry point consumes one initialized heap
permission and initializes the conserving node and actual pin atomic together.
It returns the matching creator pin, allocation token and empty observation ledger;
node identity, exact memory and drain domain are established as postconditions.
Native allocation/layout, weak memory, queue integration and destruction remain open.

Validation: Cache 653 verified, zero errors. Both new constructor mutations
(wrong drain domain and initial count two) fail verification after a clean baseline.
The ownership suite now configures 44 mutations; only these two were rerun for this
increment. TCB audit and diff checks pass. No native or remote CI run is claimed.


## Cache atomic retirement entries use the same protected allocation ledger

Added `queued_atomic` for both widths. Its constructor moves initialization's
allocation and observation resources into a node-bound actual RwLock. Final pin
release obtains that lock and produces a payload borrowing the same node. Atomic
registration uses the existing matching Current/queue locks. Recovery obtains the
node lock, derives allocation presence from the final-pin ticket, and invokes the
same pin atomic's recovery. Locked detachment recovers every exact queued allocation
in reverse pop order with borrowed full-domain DrainSet authority.

This increment does not complete normal pending-generation reclamation: scoped
observations through this lock and narrowed coverage/preparation remain to connect.
The native node layout, weak memory and destruction are also still open. Full-domain
recovery is not a replacement completion criterion for the two-generation path.

Validation: Cache 670 verified, zero errors. Four new mutations reject foreign
allocation/node identity, missing drain coverage, and loss of a recovered batch
allocation after an untouched baseline. All 48 ownership anchors are unique.
TCB audit and diff checks pass. The full mutation suite and native/Miri/Windows/CI
were not rerun for this proof-only increment.


## Cache narrows locked observation coverage and reserves the prepared queue

- A tokenized snapshot tracks the actual observation coverage/frozen state inside
  each resource-lock invariant. Final release issues a same-instance bound receipt;
  narrowing preserves all previous receipts. Prepared recovery derives sufficient
  drain coverage from the persistent receipt after reacquiring the resource lock.
- Queue preparation holds the real write handle, polls actual stripe collection,
  and preserves the entire queue on busy return. Complete drains justify narrowing
  each entry; leases are restored only after the borrow ends.
- The reservation adapter rechecks Current, prepares and reserves with the same
  borrowed handle. Both stale paths return the still-owned queue and either partial
  collection or restored controllers. Prepared detachment consumes the phase token
  for the matching drain domain and recovers the exact batch in reverse pop order.
- The complete publication/pending driver, scoped observation operations through
  the lock, native representation, weak memory and Box/Drop remain incomplete.

Validation: Cache 691 verified, zero errors. Ten queued-atomic mutations (four
existing and six new) pass after a clean baseline; three additional recheck/reserve/
collection mutations also pass after a fresh final-state baseline. All 57 ownership
anchors are unique. TCB audit and diff checks pass. No full mutation-suite, native,
Miri, Windows or remote CI run is claimed.


## Cache striped publication and pending callback composition

The Cache queue adapter now joins `prepare_reserve_collected` to the actual striped
rotation driver. Retry preserves its partial collection and transition handoff;
stale reservation restores sealed controllers; successful publication returns the
matching old-generation prepared token after shared seal/publish/reopen.

`recover_pending_striped` collects the actual pending stripes, acquires its queue,
recovers every exact allocation, and uses the production-shared callback-before-clear
macro. Pending is cleared only after recovery returns the matching ready token;
live-generation controllers remain unchanged. This proves resource recovery, not
Box/Drop. Scoped observation operations through the resource-lock node and native
representation/weak-memory/destruction correspondence remain incomplete.

Validation: Cache 697 verified, zero errors. Four new SMT mutations fail after a
clean baseline (incomplete drain, wrong prepared coverage, wrong Current queue phase,
and pending left uncleared). All eight lifetime checks pass, including the new Cache
E0382 check for pending clear before callback. All 61 ownership mutation anchors are
unique. TCB audit and diff checks pass. No full SMT mutation-suite, native, Miri,
Windows or remote CI run is claimed for this proof-only increment.


## Cache node observations retain owned admission shares

Replaced the queued node's borrowed observation ledger with a conserving ledger of
owned admission Shares and storage-backed node observation fragments. Receipt tokens
bind ledger, scope and exact memory. Ending an observation consumes its receipt and
returns the Share to the same Scope; the existing counter scope release requires
zero outstanding shares. Persistent pin retirement history excludes new observations
after final release. Node and Entry no longer carry the borrowed scope lifetime.

Node observation, completion and actual observed CAS acquisition use the same
resource lock. Initialization, final release, narrowing and prepared queue recovery
now use this owned ledger. `lookup_pin` composes actual counter admission, observation,
atomic lease acquisition, observation completion and admission release. Ended receipts
cannot be reused for pin acquisition.

Native `ResidentEntry::clone` returns a non-owning snapshot, while `lookup_pin` takes
a borrowed resident fragment. The helper is therefore a stronger-premise composition,
not the native `get_at_epoch` refinement. Split observe/acquire APIs allow retirement
between observation and acquisition; native index snapshot transfer, generation and
resident rechecks, rollback, typed scoped reads, weak memory and Box/Drop remain open.

Validation: Cache 730 verified, zero errors. The full 68-case ownership mutation
suite passed, including seven retained-ledger mutations; the two subsequently added
lookup mutations passed separately after their fresh baseline. All 70 anchors are
unique. All nine lifetime gates pass, including E0382 for using an ended observation
receipt. TCB audit and diff checks pass. No native/Miri/Windows/remote CI run is
claimed for this proof-only increment.


## Production, Loom and Verus share Cache lookup branches

Extracted `lookup_after_observation!` into the production-shared pin transition
source. Native `get_at_epoch` now uses it for eligibility, acquisition, resident
recheck, rollback pin release, domain capture, admission exit and reclaim handoff.
The Verus `complete_lookup` consumes an existing observation receipt and its scope,
uses actual pin acquisition/release and counter release, and proves the returned
lease or final retirement entry matches the observed allocation. It does not retain
a resident fragment across lookup. The Loom temporal-reclamation reader now uses
the same expression and arithmetic, including rollback quiescence after scope exit.

Index snapshot-to-observation transfer, the native metadata loads/address mapping,
and execution of native rollback enqueue/drain/destruction remain separate: the
proof accepts arbitrary metadata samples and returns the retirement handoff. Its
overflow branch is nonreturning, not a proof of native process abort. This closes
shared branch-expression correspondence, not the full native lookup theorem.

Validation: Cache 734 verified, zero errors. All six shared pin/lookup SMT mutations
pass after the clean baseline. All ten lifetime gates pass, including E0382 when
the shared macro leaves admission before pin acquisition. Native Cache tests pass
59/59 with one manual benchmark ignored, including the updated Loom model. Clippy
for cache+handles/all targets with warnings denied, formatting, TCB audit, panic
boundary audit and diff checks pass. i686-pc-windows-msvc cargo check passes (not
Windows execution). Pinned nightly Miri Tree Borrows passes cache_hit_does_not_clone_key
and hit_path_and_clear_race_safety. The race run emits parking_lot_core 0.9.12's
integer-to-pointer/provenance warning, limiting pointer-bug detection. No full Miri
suite, Stacked Borrows rerun, Windows runtime or remote CI result is claimed.


## Guarded resident entry transfers an independent observation snapshot

Added a 32/64-bit resident-cell adapter backed by a vstd RwLock. Its stored value
owns a resident pin; lookup issues the node observation while borrowing that pin
under the read guard, releases the guard, and returns an independent Snapshot.
Removal transfers the resident capability under a write handle. Snapshot completion
uses the existing production-shared lookup expression, and lookup_complete composes
actual counter admission, guarded transfer and completion without borrowing a
resident pin across the acquisition attempt.

Local quick_cache 0.7.0 source confirms get clones within its shard read-guard
expression. This motivates the transfer point but does not mechanically identify
the vstd cell with the native library entry: hashing/eviction, Clone/Drop ghost
instrumentation, native identity, metadata loads, weak memory and destruction remain
unproved. This is a per-entry guard refinement adapter, not a native index completion
claim. The proof-only change adds no production lock or snapshot overhead.

Validation: Cache 748 verified, zero errors. Six new mutations reject invalid stored
residency, wrong pin role, foreign admission, lost snapshot identity, foreign scope
and foreign lookup counter after a clean baseline. All eleven lifetime gates pass,
including E0505 for releasing the index read guard before observation transfer.
All 76 ownership anchors are unique; TCB audit and diff checks pass. No full mutation
suite or new native/Miri/Windows/remote CI run is claimed for this increment.


## Guarded lookup returns a typed owned lease

Added Node::borrow_pin and an owned Lease binding the exact node, pointer, initialized
heap permission and Lease-role pin. Its typed read borrows the pin storage guard;
release consumes that same pin through Node::release. Snapshot::into_lease and the
admitted resident-cell lookup now return the owned Lease, preserving exact memory
and owner identity, instead of exposing only the pin token.

The Verus borrow gate rejects release while a later typed reference use remains.
The native compile-fail suite now separately checks CacheLease::Deref: dropping the
lease before a later reference use fails E0505. Native field/address/provenance
mapping and actual destruction are not established by this API lifetime check;
the proof's generic allocation T is not yet the native inline CacheNode<V> layout.

Validation: Cache 758 verified, zero errors. Three new SMT mutations reject wrong
node identity, pointer mismatch and uninitialized memory. The scope-identity mutation
anchor was made specific after the new wrapper introduced a second matching clause;
it was rerun successfully after a clean baseline. All 12 Verus lifetime gates and
all four native compile-fail probes pass. All 79 ownership anchors are unique;
TCB audit and diff checks pass. No full mutation-suite, new runtime/Miri/Windows
or remote CI run is claimed for this increment.


## Inline allocation fields and value projection share native source

CacheNode's original flat field declaration now comes from cache/node_layout.rs,
with unchanged native field order, field types and visibility. The same declaration
instantiates Allocation<V> in Verus. Native CacheLease::Deref, scope reads and the
verified ValueLease share the `.value` reference expression. ValueLease borrows the
whole initialized Allocation through the existing exact-pin Lease; release preserves
the identity of the whole allocation in any resulting retirement entry.

The proof does not yet tie its separate atomic backend to the native pins field or
its opaque domain pointer to CacheLookupDomain. Sharing the declaration is not an
ABI theorem or proof of native allocation, raw pointer provenance, index metadata,
weak memory, or Box/Drop. Those remain required for completion of the original goal.

Validation: Cache Verus reports 764 verified, 0 errors. Removing ValueLease's pin
invariant fails SMT verification after a passing baseline. All 13 Verus lifetime
checks and four native compile-fail checks pass; the added lifetime case rejects
allocation release while the projected inline value remains borrowed. All six
shared pin/lookup mutations pass and all 80 ownership mutation anchors are unique;
the full 80-case SMT mutation suite was not rerun. Native cache tests pass 59 with
one manual benchmark ignored, including inline alignment/address/exactly-once-drop
and production-sharing Loom cases. Cache/handles all-target Clippy with -D warnings,
i686 Windows cross-check, formatting, TCB audit and whitespace checks pass. This is
not Windows execution or remote CI evidence.
The inline alignment/address/exactly-once-drop test also passes pinned Miri
nightly-2026-08-22 in both Stacked and Tree Borrows modes. The first invocation
stopped at existing deprecated-fetch_update build warnings; reruns used the
repository Miri task's CARGO_BUILD_WARNINGS=allow and RUSTFLAGS=-A deprecated
settings. No full Miri suite was run for this increment.


## Native final reclamation consumes retirement ownership

The native single-node reclaimer formerly accepted the raw pointer copied out of
ReclaimEntry. It now consumes the entry itself; both callers move their retirement
ownership to Box recovery. The Box is dropped in place under existing panic
containment, preserving large-value behavior and avoiding a new payload move.
The explicit unsafe precondition still requires matching quiescence or a
never-published node. This change aligns the native ownership-transfer shape with
Entry::recover; it does not itself prove their allocation identity correspondence,
weak memory, destructor behavior or the complete native path.

Validation: all five native borrow/ownership probes pass after clean baselines,
including a new double-reclamation E0382 case. Cache native/Loom tests pass 59 with
one manual benchmark ignored. No new native Miri, Windows execution, remote CI,
or full SMT mutation-suite run is claimed for this signature change.
All 14 Verus lifetime/ownership gates also pass after clean proof baselines,
including the new second-Entry::recover rejection. Cache/handles all-target Clippy
with -D warnings, the 50-reference panic-boundary audit, formatting and whitespace
checks pass.


## Native Cache drained batches retain their domain borrow

ReclaimEntries now borrows its CacheLookupDomain through reclamation. Nonempty
construction remains inside matching DrainedGeneration/ClosedDomain queue extraction;
empty deferred/failure results carry the same owner. Merging preserves an existing
batch allocation and rejects nonempty batches from a different domain before
combining their entries. The generic IntoIterator escape was removed; the private
reclaimer consumes the records while accepting the owner-borrowing batch.

The native compile-fail gate rejects dropping the domain before reclaiming the
batch (E0505 after a clean baseline). All six native ownership/lifetime probes pass.
Existing Cache/Loom tests pass 59 with one benchmark ignored; an added regression
checks two successive drains merged under one owner preserve every entry and weight.
This adds one owner reference to the cold drained-batch wrapper, not each node or
lease. It is not a theorem identifying native domain addresses with ghost domain
IDs, does not establish queue payload provenance, and does not prove Box/Drop.


## Locked Cache recovery preserves queue/payload owner agreement

The prepared and full-drain lower recovery APIs now require payload owner equality
and guarantee valid_records for the original queue alongside exact allocation
recovery. The new_queue constructor and pending striped driver already establish
the stronger condition; no new assumed identity or external body was introduced.
Cache Verus passes 764 obligations. Removing either lower API's owner condition
fails SMT verification after a clean baseline. All 82 Cache ownership mutation
anchors are unique, and the TCB audit passes. The full 82-case mutation suite now passes: each group verifies its untouched
baseline, and each mutation must fail SMT obligations rather than compilation.
Native production code is unchanged in this proof increment.


### Native foreign-owner batch rejection is exercised

A subprocess regression exercises both rejection points: a foreign first nonempty
batch, and a foreign nonempty batch appended to an existing owner batch. Both
abort; on this macOS run the test requires SIGABRT, so an unrelated assertion
failure does not satisfy the expectation. The subprocess test is excluded from
Miri. It passes with the owner-retaining merge implementation. This checks the
native fail-stop behavior, not a formal native/proof owner identity theorem.

The foreign-batch regression also passes all-target Cache/handles Clippy with
-D warnings. The owner-borrowing native code cross-checks on i686 Windows. The
inline alignment/address/exactly-once-drop test passes pinned Miri in both Stacked
and Tree Borrows modes using the repository's deprecated-warning configuration.
These are targeted checks, not full Miri or Windows execution. Full Cache ownership
SMT sensitivity validation is now 82/82; native representation and full-stack
completion obligations remain open.


## One shared owner check for Cache and Handle batch transfer

Moved the already-verified append_owned_batch expression to retirement_queue.rs;
removed its former Handle-local definition and updated the mutation gate's source.
Cache extend now uses the same owner check and append operation as Handle; the
single-batch return uses the same check with a returning operation. The macro's
operation is a tail expression, preserving append's unit result while allowing
that batch return without no-effect statements or a redundant return.

Handle Verus passes 801 obligations and Cache Verus 764; Cache native/Loom tests
pass 61 with one benchmark ignored, and the five Handle domain tests pass. The
final tail-expression adjustment passes all-target Cache/handles Clippy. Existing
native foreign-owner abort cases and shared mutation cases are rechecked below.
No full native representation, fresh full Miri, Windows execution or remote CI
completion is claimed.
The final shared expression passes all three batch mutation cases after a clean
Handle proof baseline: omitted owner check, omitted payload transfer, and transfer
before owner check. The native foreign-owner abort regression and four tests
selected by the batches filter pass (three ownership cases and one unrelated
input-identity batch test). Cache-only compilation, TCB audit, formatting and
whitespace checks pass. All 151 Handle mutation anchors remain unique; a full
151-case SMT rerun is not claimed.


## Cache retirement retains and checks its allocation domain before registration

ReclaimEntry captures node.domain at final pin release, with named pointer/weight/
domain fields. All registration paths pass through the shared owner check before
selecting a generation or invoking a queue callback. The fail-stop subprocess test
now includes a foreign-entry registration case; reaching generation selection exits
with a distinct code that cannot satisfy the rejection test. macOS requires SIGABRT.
This is native enforcement of the verified registration precondition, not a theorem
identifying native addresses with proof instance IDs. Queue metadata gains one
pointer field; node and lease layouts are unchanged.

Validation: Cache native/Loom tests pass 61 with one benchmark ignored, including
foreign first-batch, appended-batch and registration rejection. The inline payload
alignment/address/exactly-once-drop test passes pinned Miri Tree Borrows. No new
Stacked Borrows run, Windows execution, remote CI or full Verus rerun is claimed
for this native-only increment; the shared macro body and proof source are unchanged.


## Inline allocation owner enters the pin ledger through its actual field

Added a permission-backed initializer that reads the domain from initialized
Allocation<V> and passes it to Node::new. The same node_layout::domain expression
now supplies the native retirement entry's domain field. The initializer preserves
the exact creator permission; ValueLease requires allocation/ledger owner agreement
and its release preserves that owner in the returned retirement entry. This is a
local representation connection, not completion of native Box/atomic/NonNull/index
construction and identity obligations.

Cache Verus passes 766 obligations. Three targeted mutations pass after a clean
baseline: substituting the allocation address for domain, removing owner agreement,
and dropping the allocation pin invariant. The TCB audit passes. The ownership gate
now contains 84 cases; the preceding full 82-case run is not reported as a full run
of this revised suite.
All 14 Verus borrow/ownership gates pass with the stronger inline lease invariant.
All 84 mutation anchors are unique. Native Cache/handles compilation and all-target
Clippy with -D warnings pass. No new runtime, Miri, Windows or remote CI run is
claimed for this shared field-projection increment.


## NonNull boundary probe and assumption-audit repair

The installed verifier rejects this direct native-type probe:

```rust
use vstd::prelude::*;
verus! {
fn pointer(value: std::ptr::NonNull<u8>) -> *mut u8 { value.as_ptr() }
}
```

Both the type and as_ptr lack supported vstd specifications. No project assumption
or external body was added to make the probe pass, and the raw-domain allocation
adapter is not promoted to native NonNull refinement. This is a concrete remaining
library-adapter obligation, not an overall stop: allocation/control/resource wiring
work remains independently possible.

Inspection found that the existing audit missed assume_specification, axiom fn,
external type/function specifications, and assume calls split across lines. The
audit now matches sanitized whole-source tokens, rejects the assumption forms,
and requires explicit approval for external specifications. Eight audit unit tests
pass, covering multiline/comment-separated assumptions, external spec approvals,
and ignored literal/comment text. The strengthened repository audit passes with
zero local assumptions/axioms and no unapproved externals. Native and formal
implementation code is unchanged in this audit increment; no new native/Miri/
Windows/full-Verus result is claimed.


## Protected allocation owner relation survives snapshot completion

The actual vstd index lock now carries a ghost predicate on its resident allocation
permission. A checked constructor establishes the predicate; lookup preserves it
through the independent snapshot receipt and actual admission/lookup completion.
The inline allocation adapter instantiates this predicate with stored-domain/owner
equality, then wraps successful pins as ValueLease and preserves that owner through
rollback retirement. Owner agreement is no longer a fresh caller premise between
index lookup and typed lease construction.

Cache Verus passes 772 obligations. The strengthened TCB audit passes; all 87 Cache
ownership mutation anchors are unique. Targeted index/inline mutation verification
is recorded below. Native production source is unchanged in this increment; no
new native/Miri/Windows/full-suite result is claimed. Native index/NonNull/atomic
representation and Box/Drop obligations remain open.
All 12 targeted index/inline mutation cases pass after their clean proof baselines.
The new cases remove the protected allocation predicate, replace the inline index
predicate with true, or admit an unrelated allocation owner at typed lookup; each
fails SMT verification. The full 87-case suite was not rerun for this increment.


## Typed lookup reads generation under its actual observation capability

Added permission-backed immutable generation access through a Snapshot's retained
receipt. The typed lookup now acquires admission, obtains a guarded snapshot, reads
the allocation generation, and derives eligibility using the same field projection
and epoch/residency expression as native get_at_epoch. It no longer accepts an
unconstrained eligible boolean. Success guarantees the requested epoch; a mismatched
snapshot cannot yield a lease or rollback retirement. Resident atomic samples and
native backend representation remain outside this connection.

Validation: Cache Verus passes 776 obligations. Three shared-source mutations fail
SMT verification after a clean baseline: reading weight instead of generation,
omitting epoch comparison, and omitting residency. Native Cache/Loom tests pass 61
with one manual benchmark ignored; Cache/handles all-target Clippy with -D warnings
passes. All 90 ownership mutation anchors are unique. No new full 90-case run,
Miri, Windows execution or remote CI result is claimed.


## Resident observations become actual Acquire loads in the typed path

Replaced typed lookup's sampled resident inputs with direct loads of its observed
Allocation.resident. The first load is short-circuited by epoch comparison; the
second executes inside the production-shared lookup expression after successful
pin acquisition. Both use the live retained observation permission. Native lookup,
Loom reader and proof share the same resident field/load/Acquire expression.
Ghost sample evidence retains the initial-resident requirement rather than losing
it when the input booleans disappear. Generic sampled helpers remain alongside the
typed path and are not claimed to be native atomic refinements.

Cache Verus passes 786 obligations. Shared generation/eligibility mutation cases
all pass, including omission of the initial resident evaluation. Native Cache/Loom
tests pass 61 with one manual benchmark ignored; Cache/handles all-target Clippy
passes. Native store histories, pin-field correspondence and full weak-memory/
NonNull/index/Box/Drop connections remain open. No full ownership mutation-suite,
Miri, Windows execution or remote CI run is claimed for this increment.
All 15 borrow/ownership probes pass after clean baselines, including the new
E0505 rejection for releasing the allocation permission before resident.load.
The Loom temporal-reclamation test passes again after using the shared resident
load expression. Strengthened TCB audit, formatting and whitespace checks pass.


## Atomic contract probe distinguishes accepted calls from state refinement

Added tools/probe_verus_atomic_contracts.py to make the native contract boundary
reproducible against the installed verifier. Direct std AtomicBool load verifies,
but initial-value assertions for fresh AtomicBool and AtomicUsize fail SMT
verification. The permission-backed PAtomicBool control verifies the corresponding
assertion. The diagnostic labels these outcomes proved/unproved; it is deliberately
not a correctness gate that requires future verifier versions to remain incomplete.
Unexpected compiler/tool failures stop the diagnostic.

Inspection of the matching vstd revision confirms the primitive owns its native
atomic and uses SeqCst operations; the ghost constructor builds a new primitive.
There is no demonstrated adoption of the actual CacheNode field. This rules out
claiming that accepting resident.load or constructing another Pins atomic closes
field identity/history refinement. The required next implementation boundary is
one owner for the real field and its permission, with explicit native ordering.
No new assumptions, external bodies, native source changes, or full-refinement
claims accompany this diagnostic increment.


## Cache pin retry control is shared across native, Loom and Verus

Replaced the native fetch_update wrapper and independent proof loops with the same
`acquire_retry!` expansion. The backend selects the atomic implementation and
ordering; the shared control handles terminal zero, overflow, weak CAS success and
retry from its failure value. The Loom temporal-reclamation model uses this same
expansion. Ghost updates still occur only in a successful atomic operation.

Validation for this increment: Cache Verus 786 verified / 0 errors; shared Cache
pin/lookup mutation suite rejects all eight mutations, including granting a pin on
failed CAS and fabricating a zero failure observation. Native Cache tests pass
61 with one manual benchmark ignored, including Loom. Cache/handles all-target
Clippy, formatting and strengthened TCB audit pass. All 241 Cache/Handle ownership
mutation anchors remain unique; this anchor check is not a full mutation-suite run.
The native ordering remains unchanged; no native performance measurement, Windows
execution, remote CI or complete memory-model refinement is claimed.

The i686 Windows target passes cargo check for Cache/handles. The x86_64 MSVC
cross-check stops in the blake3 dependency build because this macOS environment
has no ml64.exe; it does not reach a complete Rust check and is not counted as a
pass. The local aarch64 native checks above and both Verus widths do pass.
The ten atomic pin ownership mutations also reject after a clean Verus baseline,
including both failed-CAS token grants, count conservation, owner identity, drain
coverage, retirement freezing, and the overflow/zero distinction.


## Shared final Cache pin release and weak-memory mutation gate

Production and Loom previously duplicated the final decrement/fence sequence.
They now instantiate the same `release_pin!` tail as the Verus pin-token adapter.
Both temporal-reclamation roles and the focused final-holder test use it.
The proof's SeqCst backend supplies no fence; native and Loom supply the Acquire
fence after their Release decrement. This boundary is explicit in TCB.md.

Validation: Cache Verus 786 verified / 0 errors; all ten shared pin/lookup SMT
mutations reject, including nonfinal retirement and losing final retirement.
The new `just cache-release-ordering` passes its untouched one-test baseline and
rejects both missing and late fences through actual Loom stale-value assertions.
Native Cache tests with cache,handles,bench-internals pass 68 with one manual
benchmark ignored. This includes the corrected retired-entry size expectation
for the already-added domain pointer. Clippy with those features and all targets,
TCB audit, formatting and whitespace checks pass. All 241 Cache/Handle ownership
mutation anchors remain unique; the full ownership mutation suites were not rerun.
The new Loom gate is wired into CI, but remote CI and Windows execution are not
claimed. Atomic field identity, native weak-memory refinement, allocation/Drop
adapters and the full main-path completion audit remain outstanding.


## Native Box support probe and in-place destructor validation

Probed the actual allocation/recovery APIs before attempting a new adapter.
The installed Verus proves Box::new contents and permission-guarded ptr_ref;
Box::into_raw/from_raw and Box::leak-to-pointer remain unsupported. The diagnostic
is recorded in tools/probe_verus_heap_contracts.py and adds no assumption or
external body. This prevents treating a returned HeapPermission as an already
verified native Box conversion. A proper primitive contract/implementation
connection is still required; arbitrary destructor behavior is not proven here.

Strengthened the native large/aligned inline-payload test to record its leased
address and assert that same address inside Drop, for both zero-budget and resident
cache paths. The exact test passes natively and under the pinned Miri toolchain
with Stacked Borrows and Tree Borrows, with leak/alias checks enabled. This is
runtime evidence for the actual Box path, not a theorem for generic Box/Drop.
A temporary-workspace mutation replacing native drop(node) with drop(*node)
passes compilation but fails the destructor-address assertion after a clean
one-test baseline; the production source is not changed by this experiment.
TCB audit, formatting and whitespace checks pass. No full-suite or remote CI run
is claimed for this increment.


## Reader-owned observation removes the proof lock from metadata and pin CAS

Moved the linear Cache observation into Ticket. The retained ledger keeps each
admission share and receipt bookkeeping; count conservation and drain coverage
still prove zero observations. Ticket's type invariant binds its observation to
the exact receipt memory, and Node/Snapshot explicitly retain its node identity.
Acquiring a pin and reading generation/resident now borrow Ticket directly,
without acquiring the proof-only ledger RwLock. Ending the observation consumes
Ticket and removes its share together.

Cache Verus passes 786 obligations. All nine retained-observation SMT mutations
reject, including the new missing-observation-consumption and wrong-node cases.
All 15 borrow/ownership probes pass; the resident-load probe now attempts to end
the Ticket while its allocation borrow remains live and requires E0505. The full
Cache ownership mutation suite subsequently passed all 92 mutations, each after
a clean group baseline and with a verifier failure required for rejection. TCB audit
passes, with no new assumptions or trusted adapters. Native production code is
unchanged in this increment. Registration/release/recovery locking and native
atomic/Box correspondence remain incomplete under the selected no-new-TCB policy.


## Full Cache ownership regression after reader-owned Ticket change

The complete check_cache_ownership_refinement.py run finished successfully for
the reader-owned Ticket tree: all 92 mutations were rejected by SMT verification.
This includes the queued recovery, frozen coverage, index, inline value and owner
boundaries as well as the nine direct retained-observation mutations. No source
changes were made while this suite was running. The earlier 786-obligation Cache
verification, 15 borrow probes and zero-assumption TCB audit belong to this same
implementation. This is not native atomic/Box refinement or remote CI evidence.

The next supported reduction is to execute the pin decrement before acquiring
the proof ledger lock, returning immediately for a nonfinal release. Final-pin
retirement authority must then justify freezing coverage under the lock. The
native implementation already releases nonfinal pins without a node ledger lock;
adding a trusted synchronization adapter is not authorized by the selected policy.


## Nonfinal pin release no longer takes the proof ledger lock

Moved Node::release's atomic pin decrement before ledger acquisition. Nonfinal
release returns without the lock. Only the final retirement token permits a
RetiredNode, followed by observation/coverage freezing under the ledger lock.
Removed the unused release_owned_covered helper rather than retaining a second
lock-spanning release path. The resource machine still excludes resident pins
once final retirement is issued, including the interval before ledger freezing.

Validation on this increment: Cache Verus 784 verified / 0 errors (two obligations
removed with the obsolete width-instantiated helper); all 26 queued-atomic,
lease and recovered-owner mutations pass their negative verification gates,
including the two new missing-freeze/nonfinal-retirement mutations. All 15
borrow/ownership probes and the TCB audit pass. All 245 Cache/Handle mutation
anchors are unique; this anchor check is not a rerun of every mutation. The prior
full 92-mutation Cache run applies to the preceding reader-owned-Ticket tree;
the now-expanded full 94-mutation suite has not been rerun. Native code is unchanged.

Final-release completion, observation registration/completion and heap recovery
still have proof-only ledger synchronization. Native field identity, weak-memory
and Box support gaps remain incomplete; no trusted adapters have been added.
