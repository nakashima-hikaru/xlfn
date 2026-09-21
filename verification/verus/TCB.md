# Trusted Computing Base (TCB) for Verus Formal Verification in xlfn

## 1. Overview & Separation of Verification Responsibilities

xlfn employs a multi-tiered verification architecture to ensure safe, high-throughput concurrent computation, caching, and generational rotation.

| Layer | Target Scope | Verification Engine | Primary Guarantees |
| :--- | :--- | :--- | :--- |
| **Protocol** | Whole-system & lifecycle | **Lean 4** | Generational transition safety, shutdown quiescence, quiescence certificates |
| **Protocol & Representation Model** | Concurrent ownership, dual 32/64-bit representations & state machines | **Verus** | Bit representations (32-bit i686 & 64-bit), atomic transitions, resource invariants, linear raw pointer ownership |
| **Memory Order** | Memory-ordering regressions | **Loom** | Exhaustive or explicitly bounded interleavings of the modeled atomic orderings |
| **Runtime UB** | Aliasing & undefined behavior | **Miri** | Stacked Borrows / Tree Borrows, absence of aliasing violations or memory leaks |

Formal verification with Verus mathematically proves the logical consistency and invariant preservation of the protocol models and executable transitions. In `SealableCounter`, SSOT transition-kernel refinement verifies shared macro-defined expressions instantiated in production and Verus functions. DrainGate additionally shares notification-tail and wait-loop control flow with executable Verus backend models. Neither is a proof of the production binary or the complete machine memory model. Across all primitives, assumptions and prerequisites at the verification boundary (the Trusted Computing Base, or TCB) are explicitly defined and strictly audited.

---

## 2. Trusted Computing Base (TCB)

The following components and properties are treated as **Trusted Assumptions (TCB)** outside the scope of Verus automated proofs.

### 2.1. Target Architecture & Word Widths
1. **Target Word Sizes**:
   - 32-bit architectures (`i686-pc-windows-msvc`, Excel 32-bit process): `usize = u32`, 32-bit pointer width.
   - 64-bit architectures (`x86_64`, `aarch64`, Excel 64-bit process): `usize = u64`, 64-bit pointer width.
   - Verus formally certifies theorems for both $W = 32$ and $W = 64$, ensuring machine-level bit carry and mask isolation are sound regardless of target word width.

### 2.2. External Libraries & Runtime
1. **`parking_lot::Mutex` Semantics**:
   - Mutual exclusion: `lock()` grants critical section access to at most one thread at a time.
   - Lock release: `unlock()` / `drop()` correctly yields access to pending or subsequent waiters.
2. **`parking_lot::Condvar` Semantics**:
   - `wait()` atomically releases the associated mutex and enters a blocked wait state, reacquiring the mutex upon wakeup.
   - `notify_all()` and `notify_one()` wake eligible parked waiters; notifications are not stored for future waiters. Registration/rechecking under the mutex supplies the lost-wakeup discipline. Spurious wakeups are allowed; fairness and termination are not assumed by the safety proof.
3. **Rust `std::sync::atomic` Machine Semantics**:
   - Atomic read-modify-write and memory operations (`compare_exchange`, `fetch_add`, `fetch_or`, `load`, `store`) satisfy their declared indivisibility and atomic memory semantics.
4. **Allocator & Pointer Layout**:
   - `Box::new` / `std::alloc` allocates a valid, uniquely owned memory block of appropriate alignment and size.
   - `NonNull::new_unchecked` is sound when invoked on non-null pointer values.
5. **Verus Library Memory and Token Primitives**:
   - `vstd::raw_ptr::PointsTo<T>` models exclusive initialized memory with pointer provenance; `ptr_ref` requires a matching borrowed permission. Its trusted implementation and the storage/token framework are library dependencies, not project-local proved allocator implementations.
   - The cache ownership proof deposits the actual tracked permission in tokenized storage. Generated instance-bound fragments and storage guards enforce linear access and withdrawal. The source audit does not audit all expanded library code.
6. **Compiler & SMT Solver Infrastructure**:
   - The `rustc` compiler generates sound machine code faithful to Rust operational semantics.
   - The Verus frontend verification pipeline and the backend Z3 SMT solver are sound.

---

## 3. Proved Properties

The following properties and theorems are statically proved within Verus with zero verification failures and zero `assume` directives, under the TCB boundaries above:

### 3.1. SealableCounter (SSOT Verified for both 32-bit and 64-bit)
- **`[SC-1]` Bounded Active**: The `active` reader count never exceeds `ACTIVE_COUNT_MASK` (proved for `u32` and `u64`).
- **`[SC-2]` Acquire Correctness**: Successful acquisition strictly increments `active` by $+1$, preserving `sealed` and `waiting` flags (proved for `u32` and `u64`).
- **`[SC-3]` Sealed Rejection**: If `sealed` is set, `acquire` returns `Rejected` (mapping to `None` / `Err(Sealed)`) (proved for `u32` and `u64`).
- **`[SC-4]` Release Precondition**: Release cannot succeed if `active == 0` (underflow produces `FailStop`, halting the process) (proved for `u32` and `u64`).
- **`[SC-5]` Release Correctness**: Successful release strictly decrements `active` by $-1$, preserving all status flags (proved for `u32` and `u64`).
- **`[SC-6]` BecameIdle Equivalence**: `BecameIdle` is returned if and only if previous `active == 1` (proved for `u32` and `u64`).
- **`[SC-7]` Reopen Precondition**: Reopening succeeds only when `sealed && active == 0` (proved for `u32` and `u64`).
- **`[SC-8]` Reopen Postcondition**: Upon successful reopen, `!sealed && !waiting && active == 0` holds (proved for `u32` and `u64`).
- **`[SC-9]` Retain Final Capability**: When `waiting && active == 1`, `release_without_notification` returns `Rejected`, forbidding the last permit from disappearing before the waiter notification mutex is acquired (proved for `u32` and `u64`).
- **`[SC-9b]` Release Without Notification Decrements Active**: When successful, active count is decremented by 1 while preserving status flags (proved for `u32` and `u64`).

### 3.2. DrainGate (shared control-flow refinement, 32/64-bit)

`drain_gate/protocol.rs` supplies the exact control-flow token bodies for production, Loom, and Verus. Backend expressions differ: production uses `IdleWait`, closures and Rust guard drops; Verus uses executable models in `drain_gate/src/refinement.rs`. These models have verified bodies and no `external_body` or `assume` directives, but correspondence to actual synchronization remains a TCB obligation:

- Lock/unlock model mutual exclusion and the final-release reclamation boundary. Notification must discharge its obligation before unlock; subsequent backend accesses are forbidden.
- The release model starts from an arbitrary successful-RMW observation and executes the imported SealableCounter transition kernel; it does not reconstruct the concurrent atomic history. Both widths have bridges from acquire/release/reopen to the permit model and from registration to a count-preserving observation.
- Wait interference is overapproximated by arbitrary raw-state samples, including reopen and spurious wakes. Exhausted samples repeat the current state. `exec_allows_no_decreases_clause` deliberately restricts the result to partial correctness: no termination or fairness theorem is claimed.
- The `mark_waiting` RMW call, AcqRel ordering, waiting-bit operand and pre-RMW active-mask observation are now shared by production, Loom and executable Verus backends. The backend implements the primitive fetch_or contract; the actual concurrent atomic history remains TCB. Backend/closure wiring beyond this operation, RAII guard destruction, raw pointer projections, reference lifetimes, and concurrent sealed stability across stripes remain open. The full-stripe registration/zero scan itself is shared and verified for both widths, including the zero-permit implication; idle-seal uses shared load/CAS control flow verified against arbitrary stale load samples, and the shared rollback expression preserves waiting. Multi-stripe seal/rollback control flow is now shared and verified against these primitive backends. Concurrent interference during the driver remains open; exact rollback restoration in the sequential model is not a snapshot claim about the live atomic array. A separate both-width induction proves sealed observations stable under arbitrary finite histories of shared reader transitions, waiting registration and repeated seal. Applying it to actual stripe observations still requires owner/generation evidence excluding reopen and undo throughout the interval. Existing production-reusing Loom tests and Miri tests exercise these boundaries.
- A zero observation under an open gate is a snapshot. Stable reclamation needs admission sealed and concurrent reopen excluded by the owner. No theorem here proves the complete rotating-domain/cache/handle reclamation path or a Lean-to-Verus translation.

- **`[DG-1]` Permit Liveness**: The existence of an active permit strictly implies `active > 0`.
- **`[DG-2]` No Admission After Seal**: Once sealed, no new permits can ever be acquired.
- **`[DG-3]` Quiescence On Drain**: Wait completion requires an actual zero observation; the permit invariant then implies zero permits. Sealed quiescence additionally requires seal/reopen exclusion.
- **`[DG-4]` Reclamation Precondition**: Memory reclamation capability for protected resources is obtainable only after `active == 0`.
- **`[DG-5]` Mutual Exclusion with Final Release**: The shared fast transition rejects the final waiting count; the shared slow tail releases under the lock and notifies before unlock. Applying this to actual owner deallocation requires the backend and lifetime obligations above.

The separate `permits.rs` resource machine conserves linear, instance-bound
admission fragments against an active count. Its executable u32/u64 wrappers
use the shared counter acquire/release expressions. A live fragment excludes a
zero observation of that same instance. `StripeLedgers` connects each position in the shared all-stripe scan to its
fixed gate instance and active token. Sealed-zero per-stripe histories compose
with that ledger and Cache's borrowed observation permit, including observations
made at different times. Mapping those instances to native addresses and proving
that actual histories obey the no-reopen rely relation remain composition
obligations.

### 3.3. PublishedOwner (snapshot model and typed memory permission)

The original PO snapshot theorems describe values and lifecycle predicates;
matching allocation IDs do not prove non-duplication of ownership. The separate
`permission.rs` now stores a tracked `HeapPermission<T>` in an opaque owner.
This combines initialized `PointsTo<T>` with the allocator `Dealloc` capability,
matching address, layout and provenance; zero-sized values require no allocation. Adoption requires matching pointer provenance and initialization,
borrowing uses the library `ptr_ref` guard, and recovery consumes the owner to
return both resources. `heap_permission.rs::free_uninitialized` consumes them
against the vstd deallocation contract after the value is uninitialized. Layout,
provenance and missing-deallocation-capability mutations are rejected by Verus.
The vstd allocator primitive remains trusted. Production Box allocation/recovery, unsized payloads,
Drop, and raw-reader draining are not yet mechanically connected to this owner.
The following named properties are protocol goals/snapshot results at that
remaining boundary, not completed proofs of the native methods.
- **`[PO-1]` Single Allocation Owner**: Each memory allocation has at most one owner.
- **`[PO-2]` Invariant Under Move**: Moving the owner struct preserves the identity and validity of the underlying allocation.
- **`[PO-3]` Borrow Isolation**: Shared borrowing via `as_ptr` / `as_ref` does not transfer or compromise ownership.
- **`[PO-4]` Linear Box Recovery**: `into_box` consumes the linear owner token exactly once.
- **`[PO-5]` Single Drop**: Deallocation occurs exactly once upon termination.
- **`[PO-6]` Reader-Reclamation Exclusion**: Active read permit holders and reclamation cannot coexist.

### 3.4. OperationGate (Phase 5)
- **`[OG-1]` Admission Gate Soundness**: Once gate closure begins (`is_closing() == true`), all future `acquire()` and `enter()` calls fail (`Err(GateClosed)`).
- **`[OG-2]` Operation Quiescence on Termination Wait**: Upon completion of `close_and_wait_begin().wait()`, the gate is closed and all active operations have drained (`active == 0`).
- **`[OG-3]` Guard Permit Invariant**: Every active `OperationGuard` or `OwnedOperationGuard` is backed by a positive count in the underlying drain gate.
- **`[OG-4]` Owned Guard Safe Reclamation Barrier**: An outstanding `OwnedOperationGuard` prevents owner reclamation until `release_inner` (or drop) finishes.
- **`[OG-5]` Permit Conservation Under Release**: Every guard release or manual `release()` decrements the active count by exactly 1.

### 3.5. RotatingReadDomain (Phase 6)
- **`[RRD-D1]` Single Active Generation Invariant**: At most one generation admits readers at any time; between transitions, an open domain has exactly one open generation.
- **`[RRD-D2]` Admission Through Current Generation Only**: Readers are admitted strictly through the published current generation.
- **`[RRD-D3]` Current Sealed Before Replacement Published**: The current generation is sealed before the replacement generation is published, preventing late readers from entering retired generations.
- **`[RRD-D4]` Quiescence Before Reclamation Callback**: The transition callback runs only after the old generation has drained completely (`active == 0`).
- **`[RRD-D5]` Closed Domain Never Reopens**: A closed domain permanently seals all generations and never reopens.

Native rotating/terminal certificate methods share `authorize_domain!` with
Verus. Its thin-pointer address comparison authorizes exactly the supplied
index or [0, 1] payload. Native `take_queue` additionally shares the ordered
authorize/lock/take callback protocol with executable detachment. Cache, Handle
bindings and Handle topics use it for ordinary generation queues. Its backend
proves exact payload transfer and no callback on failed authorization; actual
container/mutex semantics and callback-to-native-queue correspondence remain
separate obligations. Terminal `take_queues` likewise shares domain authorization,
ordered acquisition of both queue guards and paired payload transfer. Cache and
Handle call it; the model proves both queues become empty with their exact
payloads returned. Opaque proof certificates require a matching domain,
held transition, pending generation and idle state. Ordinary drained certificates
now borrow both executable state and a matching vstd RwLock WriteHandle, checked
against the expected lock and its fixed domain predicate. The positive path
acquires that handle, issues the certificate, detaches a batch, and consumes the
handle to restore state. Cache instantiates the path with typed retirement
entries. The library handle requires explicit release; this is not a proof of
native guard Drop, panic unwinding, fairness, or parking_lot implementation.
The backend serializes the complete Rotation state, whereas native reader
atomics can change outside the transition mutex. Its state/guard representation
and interference frame remain open, including terminal certificate composition. Address
equality is not used to manufacture memory or allocator permissions.

Rotation's linear `GenerationLedgers` connects selected raw-counter acquire and
release to fragments from fixed gate instances. Cache imports this proof module
and consumes the same fragment type in its scoped-observation exclusion lemmas.
Thus a matching generation-zero observation excludes that Cache observation.
The native striped representation and the retirement queue's coverage of all
relevant observations remain unproved caller/composition obligations.

### 3.6. ServiceSlot (Phase 7)
- **`[SS-1]` Single Service Ownership**: Sole ownership of `PublishedOwner` / `Box<R>` is strictly conserved across `Ready`, `SealingTxn`, `TeardownFaulted`, and `ServiceSeal::Present`.
- **`[SS-2]` Publication Validity**: The fast-path published pointer is non-null if and only if the slot is `Ready` and readers are open.
- **`[SS-3]` Withdraw Before Drain**: During `seal()`, the published pointer is nullified and readers are sealed prior to waiting for reader drain.
- **`[SS-4]` Quiescence Precondition for Box Extraction**: `into_box` is invoked only after `readers.active() == 0`, ensuring race-free extraction.
- **`[SS-5]` Fault Isolation**: Any failure during initialization or teardown transitions the slot to a fault state (`InitFaulted` or `TeardownFaulted`), preventing invalid state reuse.

### 3.7. CacheLease & Temporal Reclamation (shared pin kernel + model)

Pin acquire/release classification is instantiated from shared production
expressions for u32/u64 (`cache/pin_transitions.rs`). All pin additions, including
resident/flight anchors, use checked acquisition. Production release still
performs a Release `fetch_sub` first; a previous zero aborts rather than returning
normally, and only the final pin takes the Acquire fence. The atomic/fence
implementation and the caller wiring are outside the pure arithmetic proof.
`pin_ownership.rs` additionally conserves Creator/Resident/Flight/Lease fragments
and observations in instance-bound tokenized storage. Typed pointer borrowing
requires the matching initialized `PointsTo<T>` guard borrowed from a
`HeapPermission<T>`; storage and the final-pin retirement ticket preserve the
allocator capability alongside memory. Withdrawal requires the unique retirement
ticket, zero pins and zero observations. Both-width wrappers connect this ledger to shared
pin arithmetic. Four ownership mutations fail at the verification stage.
`scope_ownership.rs` additionally retains a borrowed linear gate permit alongside
the node observation. Its typed borrow uses that node's stored memory guard;
the same permit may cover multiple nodes. Allocation fixes the node's domain
as an immutable set of gate identities. Scoped construction requires membership,
and its type invariant preserves that association during typed borrowing.
`initialize_node` deposits actual memory ownership and returns one creator pin.
The native CacheNode domain pointer and its generation/stripe gate identities
still need a representation mapping to that set; membership is not inferred
from a raw pointer.
This does not yet prove the production source of those fragments: resident-index
observation, domain permit coverage, retirement queues, and actual struct/drop
wiring still require composition. The typed retirement wrapper now carries the unique final-pin fragment through
the shared generation-registration loop. Its generic executable queues conserve
payloads across retry and one append. Recovery consumes that fragment together
with matching zero-pin/zero-observation ledgers to withdraw the original heap
permission. Native SmallVec/closure representation and the queue's coverage of
all relevant reader generations remain open; registration does not imply zero
observations. The new opaque `ObservationLedger` conserves the unique observation
counter with a map of real borrowed-permit observations. Whole-domain sealed-zero
histories covering all assigned gates exclude every possible entry and derive
zero observations; a recovery wrapper then consumes the actual retirement
fragment. Initialization, observe, end and typed borrow preserve this ledger.
Final-pin release can now freeze the ledger; observation completion preserves
its narrowed coverage and no further observation can be inserted. The scalar
rotation ledger proves previous-generation exclusion before the shared
begin/publication expression. Active-count preservation keeps the same gate
ledgers valid afterward, and pending-generation idle derives zero observations
without requiring the reopened current generation to be idle. This proof does
not infer which native queue entries were frozen/narrowed before publication.
A verified observe/end/release witness checks that this resource API can really
consume admission after observations end while retaining the node ledger.
`rebind_empty` removes the obsolete borrow lifetime only at a conserved count of
zero and preserves the same counter token, domain, frozen state and coverage.
Native scope/index representation, striped generation identity, and the actual
queue publication cutoff remain open. The temporal propositions below remain abstract-model results at
those boundaries.

- **`[TR-OBSERVE-1]` Pointer Observation Soundness**: Observing a raw cache node pointer implies the object is live (`Published` or `Retired`), never `Reclaimed`.
- **`[TR-LEASE-1]` Lease Pin Safety**: An active `CacheLease` pin strictly prevents node reclamation, guaranteeing safe shared dereference.
- **`[TR-ADMISSION-1]` Admission Coverage**: Observations require an active admission (`observing > 0` implies `admissions > 0` in the abstract model). One scope may protect multiple observations; no cardinality bound is claimed. Matching each production observation to its domain/generation permit remains open.
- **`[TR-RECLAIM-1]` Drained Reclamation Precondition**: Transitioning a cache node to `Reclaimed` requires all admissions, observers, and pins to be zero (`admissions == 0 && observing == 0 && pins == 0`).
- **`[TR-NO-UAF]` Fundamental Temporal Safety**: In any valid system state, holding any capability (pin or observation) guarantees `status != Reclaimed`.

### 3.8. HandleReadDomain & Publication (Phase 9)
- **`[HD-1]` Admission Isolation**: Closed admission rejects new domain readers.
- **`[HD-2]` Binding Retirement Before Reclamation**: Withdrawing a binding adds a queued obligation and one unit of debt.
- **`[HD-3]` Generational Quiescence**: Detachment requires the selected generation sealed with zero readers.
- **`[HD-4]` Destruction Completion**: Detachment transfers obligations from pending queues to destruction in flight. Debt equals both pending counts plus in-flight destruction. Only completed destruction discharges debt.
- **`[HD-5]` Destruction Barrier**: Successful final completion requires closed admission, zero readers, empty queues and no destruction in flight; zero debt follows from conservation, not assignment.

The native completion tail and both-width executable backend instantiate
`handle/domain/protocol.rs`: destruction returns, debt is discharged, completion
is locked, waiters are notified, then the guard is released. The model consumes
its owned Vec and issues a linear receipt bound to its batch instance. Discharge
consumes that receipt; a foreign or reissued receipt is rejected. The executable
result refines the abstract destruction-in-flight debt transition.

This does not verify arbitrary Rust `Drop`. Verus does not support the explicit
`core::mem::drop` call here; the backend uses a lexical ownership scope to model
normal destructor return, and no assume_specification was added. The native
backend uses `drop(records)` and retains its existing panic containment in
ObjectArena. Actual Box/raw-pointer destruction, native batch/record identity,
native concurrent queued/debt representation and memory-order refinement,
completion mutex semantics, and binding/topic/object ownership composition
remain open. Arithmetic itself now uses shared 32/64-bit checked expressions:
production feeds them to AtomicUsize::try_update and aborts on FailStop. The
completion proof calls the same subtraction kernel; conditional count bridges
relate completed enqueue/detach boundaries to the corrected debt model. These
boundary premises do not claim queue length equals the queued hint at every
intermediate instruction. CAS/RMW semantics remain a library boundary. Debt
discharge retains Release success ordering; queued updates and failure loads
remain Relaxed.

Native `DrainedBindings` is distinct from pending queue storage. Nonempty batches
are constructed at certificate-authorized ordinary/terminal detachment, borrow
their HandleReadDomain, and invoke the completion tail from Drop against that
owner's counters. Merging checks owner identity through a shared macro and moves
all payloads, leaving the source empty. Verus proves exact generic Vec payload
transfer and no mutation on a rejected owner; it does not yet prove the native
SmallVec/certificate/Drop representation of this wrapper. Native Rust's ownership
and borrow checks enforce move and owner-lifetime restrictions separately.
The executable Handle Batch now has private fields and borrows a DomainOwner.
Its nonempty constructors call the imported ordinary/terminal rotation detachment
functions; they preserve exact payload sequences and verify that the queue's
rotation identity matches the completion owner's embedded rotation identity.
The two addresses are deliberately separate: the native HandleReadDomain address
is not its embedded current-generation atomic's address. Their native mapping
remains a representation premise, not a fact derived from numeric equality.
BindingOwner carries an actual initialized LinearOwner/HeapPermission and a
borrowed domain identity. The shared registration loop preserves it, certified
batches transport it, and BoundCompletion retains the owner borrow while invoking
the existing modeled destruction/completion tail. Native BindingRecord fields,
arena ownership, raw-reader exclusion, SmallVec and arbitrary Drop refinement
remain open; transporting heap permission does not prove native deallocation.
The existing native/Miri blocking-destructor regression validates the actual code
path separately; it does not turn the modeled destructor effect into a theorem.


Native HandleDomainWitness now contains an actual CallScope borrow (or a borrowed
owned permit in tests), replacing the unchecked raw-pointer/PhantomData constructor.
BindingReadLease retains that witness throughout scoped access. Scope witnesses
are constructed privately only after finding or inserting the domain admission.
The shared `call/permits.rs` insertion expression conserves the complete ordered
sequence of permits. Its Verus instantiation uses nonduplicable admission tokens
and proves that each previously admitted gate remains present. Allocation for
Single-to-Multiple promotion precedes moving the old permit; later additions
append in place. No scope operation removes retained permits before scope Drop.
This proves the shared storage transition and strengthens native Rust lifetimes;
it does not establish the full RefCell/domain-address/atomic-ledger representation
or derive native raw binding/arena memory permission from those lifetimes alone.


The production scoped BindingTable read now shares `binding/protocol.rs` with
Handle's executable `reading.rs`: authorize domain, load publication, reject an
empty slot, borrow the loaded pointer, reject mismatched identity or non-Live
observation, and construct a reader retaining admission. The verified branch for
native abort has a distinct FailStop outcome; it is not a successful native
return or a missing-slot rejection. Domain checks use address equality, matching
native thin-pointer comparisons. `borrow_observed` instead requires the exact pointer
(including provenance) matching an initialized heap permission and invokes
vstd::raw_ptr::ptr_ref with that permission. Removing this match fails verification.

The read backend now borrows a storage-backed Observation, not the publication
owner. Observation holds an actual gate-permit borrow and a nonduplicable memory
observation token. Publication ownership can move into a retirement token while
that observation remains alive. Recovery consumes the retirement token and
requires the exact allocation's zero-observation count. A positive executable
witness retires before invoking the same shared read, then ends its observation.
The retired payload passes through shared generation registration with matching
queue-owner identity. Its observation counter is now encapsulated by an exhaustive
ledger; composing native queue detachment with that representation remains required.

Its record ID and sampled Live flag still model observations, not native AtomicU8
or page publication semantics. Acquiring the observation requires a published
ownership token at the linearization point. Connecting a native AtomicPtr sample
(including loads concurrent with withdrawal) to that acquisition, proving native gate/address coverage, and mapping every native reader to the
ledger remain open. The storage proof does not infer those premises from a non-null
pointer, and must not be presented as a completed weak-memory adapter.


Handle's covered allocation now exposes its observation count only through an
ObservationLedger whose private map conserves every borrowed-permit observation.
The shared read can borrow an entry from that ledger. Actual sealed-zero stripe
histories excluding every covered permit derive a zero count for recovery;
zero is not supplied as an independent caller assumption on this covered path.
Retirement consumes publication ownership and freezes observation additions.
Before shared rotation publication, the previous idle generation is excluded
without discarding observations. Narrowed coverage is retained across generation
reuse; pending-generation idle then excludes all remaining observations, even
with active readers in the new current generation. Resource recovery consumes
that allocation's retirement token and the same ledger's derived zero count.
The generated lifetime witness ends observations and then consumes admission,
preserving the counter for later recovery. Native AtomicPtr linearization,
per-reader ghost-ledger correspondence, queue cutoff and stripe address mapping
remain representation obligations; these resource theorems do not prove those
native premises.


`atomic_publication.rs` now instantiates vstd::atomic_ghost::AtomicPtr with a
predicate relating its actual stored pointer to the unique published binding
resource. A load's atomic ghost block issues an owned admission share from the
call Scope, registers an IndexedObservation, and increments the same binding's
observation count. The registry retains the share; the returned Read owns the
memory observation and linear reader-index ticket and borrows its Slot; it does not require an externally supplied
loaded-pointer/memory equality premise. CAS clearing the pointer moves the
publication resource to retirement atomically; earlier readers still guard the
stored memory. Read::end returns the observation through the atomic invariant
and recovers its exact scope's admission share; end_in_scope completes that share.
Null/foreign/stale reads leave no new scope share, while accepted reads add one.
The production-shared read expression uses this load, borrows the real memory,
checks record identity and the modeled Live sample, and preserves admission in
an accepted reader. A positive executable path clears, copies the previously
loaded value, and only then ends observation.

The backend now permits clearing and republishing a different allocation into
one slot. Per-allocation counters live in a persistent registry, and each Read
retains its original instance and registry receipt. Registering a replacement
preserves earlier counters; ending an old reader decrements only its original
allocation. The modeled FailStop result retains the unpublished ownership bundle for outcome
accounting; it does not claim native execution resumes after abort.
CAS retirement uses the actual removed pointer's provenance; equality of its
address to the expected address does not imply equal provenance. The verified
republication witness reads the old value after attempting replacement.

Recovery now consults that same per-allocation Counts token inside an atomic
no-op ghost block. It yields the exact HeapPermission only for a registered
allocation with zero observations; otherwise it preserves the exact retirement
token. A live Read from the same slot proves the count positive and excludes
recovery, even when publication has moved elsewhere. The resource-level
observe/retire/reject/end/recover composition uses one counter throughout.
Its Result is tracked ghost state, not a runtime readiness test or a proof that
native drain has completed. Connecting native drain histories to this registry's
zero counts remains open; the separate ObservationLedger cannot be substituted
as an independently supplied zero counter.

A retained-admission resource now stores the actual linear gate permit and
issues owned observation shares. The original permit can be withdrawn only
after all shares return; each live share guards that permit and proves the same
gate active count positive. RetainedCounts wraps the existing Counts registry
with an exhaustive per-allocation share map, preserving count == map length.
Sealed drain histories covering the mapped gate identities exclude every share
and derive that same Counts token's zero before recovery, for both word widths.
A constructed allocation/admission/observe/retire/end/release/recover witness
returns the original heap permission. SlotGhost now contains RetainedCounts and
uses it for load/end/recovery. Reader-index tickets carry exact gate/scope identities;
persistent index receipts tie their issuing index to the same allocation, so end
derives membership under interference instead of assuming the share-map contents.
Atomic retirement preserves its counter-registry receipt alongside the original
queue payload. recover_after_histories_32/64 opens that same atomic invariant,
checks the retirement registry, derives zero from the retained shares and sealed
histories, and recovers the exact heap permission. This closes the backend's all-stripe drain-to-counter-to-recovery composition.
Retirement now also issues a persistent history token excluding republication.
The retained registry consumes this evidence before narrowing: live allocations
retain full coverage, while a retired allocation may exclude the previously idle
generation. A persistent bound is tied to the same reader index and allocation;
future operations can only preserve or reduce coverage within that bound.
Atomic narrow_and_begin_32/64 derives the bound before executing the shared
generation publication expression. recover_after_pending_32/64 opens the same
atomic invariant, validates that bound and derives zero from pending-generation
idle alone. The current generation may remain active. A composed executable
witness recovers the old allocation and then dereferences a live current-generation
Read from the same slot. The scalar GenerationLedgers helper still has two logical
gate identities. A separate striped entry path now uses the exact finite identity
set of each StripeLedgers array, with no representative-gate collapse. It requires
complete disjoint kept/idle generation domains, matched raw stripe states and
sealed idle histories. Every excluded gate must be covered by those idle histories;
recovery validates the persistent bound against every pending stripe. The other
generation may retain a live Read. Both paths use one narrow_from_exclusion state
update. The striped path still requires an adapter connecting its supplied arrays,
histories and identities to native generation selection/publication-barrier control,
CallScope addresses and the retirement queue cutoff. This is backend composition,
not a complete native rotating-domain refinement.

The enriched AtomicPtr RetiredRecord now travels through queued_retirement rather
than requiring a separate LinearOwner queue payload. Retirement binds the actual
record, slot counter/index registries and completion owner. Shared registration
preserves the exact record through generation recheck and append; ordinary and
terminal certificate detachment preserve the same resources. Before publication,
prepare_locked prepares every entry of the selected OLD queue while preserving
all allocation records and the other queue. Its lock precondition is the existing
Registration protocol state (held == current, not both_held), not ownership of a
native mutex guard. It does not yet perform or prove the publication step.
Preparation and recovery traversal are verifier-side resource adapters; only
registration and certificate detachment instantiate production-shared control.
The ordinary_recover composition detaches an authorized batch and recovers every
record using its persistent bound and matching pending-stripe histories. The
returned RecoveredBatch owns the exact heap permissions, in reverse batch order,
and keeps the completion-owner borrow and original work count. It does no user
destruction, notification or debt discharge. Native queue lock ownership, the
actual barrier-to-publication driver, live generation/history correspondence and
Box/Drop completion remain open; no new assumption covers them.

A separate barrier_ownership backend now acquires an actual vstd queue WriteHandle
and issues HeldBarrier borrowing that handle, queue state and matching lock.
Its invariant checks the lock predicate and handle identity; publication requires
the matching domain and OLD/current queue index plus the borrowed transition
handle. The production-shared publish_release expression performs publication
before consuming the queue handle. Reversing that shared order now fails Rust
borrow checking before SMT; a separate model mutation still checks the ordering
preconditions at SMT level. Native publish_then_release_barrier similarly passes
&barrier to its callback before Drop, so early release is a native E0382 error.
This proves lifetime fencing and a verified-library lock composition. The queue
predicate now also retains a per-payload invariant. queued_retirement constructs
that predicate from allocation validity, completion owner and the complete gate
domain. begin_prepared acquires the queue, derives those payload facts from the
lock invariant, prepares the same records while the old generation is current,
publishes with HeldBarrier borrowing the same queue handle, then returns the
prepared records to that lock. Every record identity and the queue length survive;
the returned ghost sequence describes preparation at publication, not a claim
that the queue remains unchanged after unlocking. Preparation after publication
is rejected because the old queue no longer matches current and pending is set.
This closes that resource-backend composition. locked_detachment additionally
instantiates the shared ordinary/terminal authorization-and-take control using
actual queue WriteHandles. The selected index and owner must match each protected
queue. Single detachment returns the exact pre-take records; terminal extraction
acquires both handles, moves both vectors out before releasing either, and
preserves their generation order and payload predicates. Withdrawal now records
the protected source sequence, owner and generation before extraction. Its
invariant equates the actual vector to that source. LockedBatch carries the
snapshot through the Handle wrapper, and into_batch exposes equality with both
the snapshot and the wrapper's records. bind_withdrawal must preserve the input
receipt's entire sequence; replacing the vector with an empty one fails even
though a per-element predicate alone would hold vacuously. This is evidence about
the state captured under the handle, not a snapshot of concurrent queue contents
before lock acquisition or after release. Handle Batch adapters retain the
completion owner. terminal_recover_locked takes both certified batches
and recovers their AtomicPtr heap permissions using histories covering the full
allocation gate domain. The per-batch loop preserves exact allocation mapping
and work count, without claiming user destruction or debt completion.
Ordinary pending-only recovery now carries linear preparation authority across
unlock/relock. Each queue predicate identifies a phase instance and the protected
state owns its state token. A ready token allows one preparation; freeze consumes
it and issues a prepared token for the exact bound. While that token exists the
lock invariant requires every payload to satisfy the prepared predicate for that
bound. The reset_empty adapter requires an empty queue before consuming the prepared
token and returning ready. Reacquisition derives preparation from the same phase token,
not from a boolean supplied by the caller. detach_prepared preserves the ticket
on domain rejection and returns a fresh ready token after successful extraction.
recover_prepared_locked validates the pending stripe domain against the ticket,
recovers the recorded allocations and returns ready only after recovery returns.
This closes preparation persistence in the resource backend. Registration on
this lock backend is connected below; native parking_lot/CallScope addresses,
actual generation/stripe histories, weak memory and Box/Drop remain open. Native queue reuse
must still be tied to the transition driver; the verifier-side preparation and
recovery traversals are not native binary proofs.

Production Handle and Cache queue append/take operations now instantiate
crates/xlfn/src/retirement_queue.rs. The same append_retired expression is used
by the registration backend and append_ready under an actual queue handle plus
matching ready authority; take_retired is used by ordinary, terminal and prepared
locked extraction. Default constructs the empty replacement before mem::swap,
matching the previous mem::take behavior. Production retains SmallVec storage;
the executable verifier uses Vec. Thus the shared ownership-moving expression
is checked against the Vec contracts, while SmallVec implementation/layout and
its native representation correspondence remain outside that proof. This does
not by itself prove that a sampled current generation supplies ready authority;
the following resource-level atomic connection addresses that separate obligation.
No locking, debt/queued ordering or publication memory ordering is changed here.

DrainGate's atomic_counter supplies actual vstd AtomicU32/AtomicU64 CAS backends
for the shared acquire/release transition functions. Its atomic invariant ties
the masked machine count to the same admission instance's linear active token.
Only a successful acquire CAS issues a permit; failed CAS attempts preserve
ownership, and release consumes the caller's matching permit only on success.
The backend exposes FailStop as a classified result; mapping it to native abort
is still the native adapter's responsibility. This backend is not the production
wait/notify path. Its seal/reopen resource adapter is described below; the
native driver correspondence is still open.

The counter now has a second token instance for lifecycle control. Its atomic
sealed token agrees with the real sealed bit and with a unique external control
token. Construction returns that token; seal requires mutable control and sets
both via fetch_or. Reopen uses the production-shared sealed-zero transition in a
CAS loop, updates control only on success and preserves it on rejection. Ordinary
acquire/release preserve sealed-bit agreement. observe_control derives the real
loaded bit from the matching borrowed control. acquire_controlled passes that
borrow through the same acquire loop used by ordinary acquire and proves sealed
control always yields Rejected. It is not a separate transition twin.

The atomic_rotation resource driver now places both counter control tokens inside
an actual library transition lock. Its predicate binds their authority identities
and requires the pending generation to remain sealed. With the matching transition
handle and reserved queue, it instantiates production begin_rotation,
publish_reopen and publish_release: seal old, mark pending, publish the actual
Current, reopen next, then release the queue handle. Both gates are required sealed
at publication, and the reopening expression is reached only after publication
returns. Failed reopening takes a non-returning fail-stop model branch, matching
native abort's lack of normal continuation, not a termination guarantee.

This is a conditional resource path: the caller supplies the matching two counters
and the next sealed state. Native transition-mutex/address correspondence and
all-stripe ownership are not derived. Handle's prepare_publish_atomic now requires
the held transition state/handle and mapped counters and invokes this combined
driver after preparing its records. Domain/stripe drain certificate composition,
Condvar progress and weak-memory refinement remain open.

The atomic counter now supports try_drain on a held sealed controller. At a real
zero load it moves both the active token and controller into a verified storage
state machine and returns an opaque DrainLease. The atomic invariant records the
frozen phase and requires its raw count to remain zero and sealed. A nonzero load
returns the unchanged controller. While frozen, a purported matching controller
contradicts uniqueness of the stored controller; a live permit/share contradicts
the stored zero active token. Acquire cannot succeed on the frozen sealed word.
Thus the existing atomic operations remain verified in both phases.

DrainLease lends the exact zero active token and proves exclusion of a matching
owned admission share. restore consumes its ticket, restores the same active token
inside the atomic invariant, and returns sealed control. A tracked borrower of
zero authority prevents consuming the lease until that borrow ends. A dedicated
compile-fail check uses an actual tracked argument after restoration; merely
remembering a spec value does not extend a resource borrow and is not treated as
that check. This is a single-counter stable drain resource. Transition pending
state, executable stripe collection, domain certificates and existing Handle/Cache
drivers must still adopt this authority instead of caller-supplied histories.
Native Condvar/atomic ordering and representation remain separate obligations.

DrainSet now owns an arbitrary set of actual DrainLeases keyed by gate identity.
Insertion and removal preserve the exact lease values and all other entries, so
collecting authority does not erase restoration identity. Its exclusion operation
borrows the stored lease for a matching live Share. RetainedCounts uses this to
prove zero observations whenever the leases cover that allocation's coverage set.
Slot recovery opens the same actual AtomicPtr registry used by load/end and
returns the exact retired heap permission. There are full-domain and persistent-
bound paths; narrowing the bound can also exclude gates through actual idle leases.
The bounded path supports borrowing a still-live current-generation reader after
old-allocation recovery. Queued Retirement adapters preserve the original allocation
through full and prepared recovery.

The new atomic_stripes collection executes the production observe_stripes scan
against a vector of actual counters. Each successfully drained stripe transfers
its controller into an owned DrainLease; busy stripes retain their controllers.
Repeated polls retain completed stripes and retry busy ones. The ready result is
equivalent to every mapped stripe owning its lease. The borrowed drain view is
available only after that condition holds. restore_all returns every mapped lease
to the corresponding counter, and into_controls yields matching sealed controllers.
The counter-vector identity and initial sealed controller map are caller premises.
The controller map now has exactly the vector's finite index domain, and the
collection invariant forbids controllers or drain leases outside the mapped
indices/gate identities. The complete borrowed DrainSet equals the vector's gate
set, not merely a superset. into_controls proves no drain leases remain and
returns exactly the mapped sealed controllers. These constraints prevent unrelated
linear resources from being silently hidden in the collection or discarded with
it; they still require a native/transition constructor to supply the matching map.
This establishes collection/restoration, not native array/address correspondence.

An exact controller map with arbitrary sealed flags can now drive
atomic_stripes::seal_all against actual counters. The production StripedDrainGate
and Loom unconditional seal scan instantiate the same seal_all_stripes expression.
The verified adapter preserves controller-to-counter correspondence and makes
all mapped controllers sealed, producing the premise required by Collection::new
and reopening. It still needs composition with the transition-owned generation
maps rather than receiving those maps as standalone caller premises.

The returned controller map can now drive atomic_stripes::reopen_all against
actual counters. Production StripedDrainGate and its Loom backend use the same
reopen_stripes expression as this verifier adapter. The adapter preserves the
map domain and counter authorities, opens precisely the returned prefix, leaves
the rejected stripe and suffix sealed, and preserves unrelated map entries.
The shared loop can stop early only after an actual reopen rejection. Native
StripedDrainGate retains its existing lock and all-stripe sealed/idle precheck;
this new proof covers the subsequent ordered reopen scan. It does not yet connect
the transition lock's arbitrary-stripe ownership to publication and this scan.

Lease-based preparation cannot simply receive both an idle lease and its reopen
controller: that would require duplicate ownership. Handle's new
prepare_collected_records now polls the actual collection, borrows its complete
DrainSet to narrow every queued record's persistent bound, ends that borrow and
restores the matching sealed controllers before returning them. A busy result
preserves every record exactly and returns the partial collection for retry.
Every excluded gate must name a counter in the collection. Successful preparation
preserves allocation identity, completion owner, gate domain and record count.
prepare_collected_queue derives these payload premises from the matching library
queue lock's invariant while borrowing its actual WriteHandle. It requires an
open preparation phase, retains the phase instance and queue identity, and
restores the lock invariant on both success and busy return. The handle remains
with the caller for subsequent reservation/publication. prepare_reserve_collected
now borrows that handle across actual current recheck, collected preparation and
Current.reserve. It returns the exact record sequence identities in the reserved
queue with its persistent bound, together with restored matching controllers.
Stale selection or busy drain returns the owned queue and partial collection;
reservation rejection after successful preparation returns the queue and restored
controllers. No branch duplicates or drops the caller's linear collection state.
Queue acquisition and the transition-owned stripe controller handoff into actual
publication/reopening remain outside this entry.
This connects collection to record preparation and controller restoration; the
existing prepare_publish_atomic entry still uses history inputs. Connecting the
returned controller map to the transition driver, then publishing and reopening,
remains a separate obligation.

This removes caller-supplied history sequences from these new recovery/narrowing
adapters. It does not yet replace the existing Handle preparation loop's history
inputs, compose the collection into transition/callback control, or switch Cache
recovery. Domain/transition/callback integration and native representation remain
open. Merely importing these proof modules does not establish those connections.

acquire_scope retains that actual atomic-issued permit in the existing Scope
resource. release_scope requires all observation shares to have returned; a load
with a matching live share proves the actual masked counter is nonzero. Handle's
32/64-bit admitted_read composes acquire_scope with the actual AtomicPtr load and
heap-backed borrow. An opaque AdmittedRead owns its Scope and Read; ending it first
returns the observation and then releases the counter. An empty slot releases the
unused admission too. Counter selection and its membership in the slot gate domain
remain explicit caller preconditions; native stripe/address mapping and generation
selection are not derived by this adapter. Both atomics use vstd SeqCst semantics.

Current selection and registration now use an actual vstd AtomicBool and actual
queue WriteHandles. The current atomic invariant owns either the selected queue's
ready token or its protected phase-state token during a publication reservation.
A registration recheck holding that queue cannot coexist with the latter token:
uniqueness of the phase-state token rules it out. A matching load therefore derives
ready state without a caller-supplied current/ready implication. The production
register_retired retry expression selects, locks, rechecks, releases stale handles
and appends the exact payload using this backend. Handle's register_atomic adapter
preserves the exact enriched retirement entry through that path.

The publishing producer of this invariant is also checked: reserve moves the held
queue's phase-state token into the atomic invariant and retains exact prepared
payloads in ReservedQueue; publish_reserved consumes its reservation, installs the
other queue's ready token at the atomic store, and returns the old protected state
and prepared authority. The raw store helper is restricted to the rotation module;
the former queue-only public publication entry point has been removed. Its only
current store caller is the atomic_rotation publication helper, requiring sealed
controls, pending state and the held transition handle. Handle's 32/64-bit
prepare_publish_atomic acquires the queue while borrowing that transition handle,
rechecks current, prepares every retirement's coverage, reserves, and invokes the
combined seal/publication/reopen/barrier-release driver. Success returns the
prepared snapshot/ticket and proves old sealed, next open and pending old. Stale
selection returns next-ready authority and leaves transition state unchanged.
This connects the resource-level publication path; it does not identify the
native AtomicU8/parking_lot representation or supply persistent drain histories
and generation-reuse completion. Safety is partial correctness; fairness and
termination of retry/lock acquisition are not proved. No assumption or project
external_body bridges these remaining boundaries.

This backend remains SeqCst. The pinned vstd atomic implementation uses SeqCst
for load/store/CAS; native PublishedBindings uses Acquire, Release, and
AcqRel/Acquire. Publication now uses the same empty-check/store expression as native code.
A linear writer view agrees with the atomic view; every pointer update requires
both resources. The verified writer-lock wrapper owns that writer view, acquires
it through a real vstd WriteHandle, performs publication/removal, and returns it
on release. It proves the resource-level check/store serialization, including
readers that update observations between the two atomic operations. Occupied
publication is a distinct FailStop outcome corresponding to native abort.
Native pointer mutations now require a private PublicationWriter that exclusively
borrows the actual parking_lot write guard. Construction checks that the guard
locks this BindingTable; insert/remove require mutable access to the capability.
Native compile-fail checks reject both releasing the guard before publication
and obtaining simultaneous writer capabilities. These Rust lifetime guarantees
and runtime identity checks do not yet establish the mapping from RegistryState
and native publication slots to the verified ghost writer authority. That mapping
and weak-memory refinement remain open, as do AtomicU8 Live sampling and the
native striped-generation and queue instantiation of retained admission coverage. Allocation supplies the real memory resource,
but native Box allocation/Drop and actual gate/address mapping remain separate
obligations. No new project external_body or assumed atomic specification is
introduced to hide those differences.

---

## 4. TCB Governance Rules

1. **Zero-Assume Policy**:
   - In both production code and Verus proofs, project-local `assume(...)`, `assume_specification`, and `axiom fn` declarations are prohibited, including tokens split across lines (enforced to be 0 by CI).
   - Any temporary assumption requires an explicit justification, an issue reference, an expiration deadline, and approval in the TCB allowlist.
2. **`external_body` Allowlist Management**:
   - External bodies and external type/function specifications must be explicitly registered in `tools/verus_external_allowlist.json`.
3. **Continuous Integration Gates**:
   - `just verus`: Automated execution of all Verus proof crates, requiring 0 verification errors.
   - `just verus-audit`: Automated inspection ensuring 0 project-local assumptions/axioms and 0 unapproved external bodies or specifications.


### Arbitrary-stripe transition-owned publication

striped_rotation now owns both exact lifecycle controller maps inside an actual
library transition RwLock. The invariant binds them to the configured counter
vectors, requires distinct gate identities across both generations, and keeps a
pending generation sealed. Its begin adapter requires the matching transition
handle, a matching reserved queue, and coverage equal to the old generation's
actual gate set. It instantiates production's seal/pending/publish/reopen/release
expression with the actual all-stripe seal and reopen adapters and Current's
resource-owning atomic store. Successful return establishes old sealed/pending,
new open, and the prepared ticket's identity and coverage bound. Partial reopen
uses the existing nonreturning fail-stop abstraction; it is not normal completion.

The former scalar atomic_rotation remains used by Handle's existing entry.
CollectionHandoff now consumes State and transfers only the next map into an
actual Collection, retaining the other map and borrowing the matching transition
WriteHandle. Its restore requires exactly the next counter map and reconstructs
State with no pending generation and the untouched old map. While collection owns
the next controllers, no publication-ready State exists.

Handle's prepare_publish_striped acquires the actual queue, creates this handoff,
composes current recheck and collected preparation/reservation, restores State,
and invokes striped_rotation::begin. Busy/stale-before-preparation returns the
handoff, partial collection and unchanged next-ready authority; rejection after
preparation returns restored State and next-ready authority. Successful return
preserves the old-generation gate bound in the prepared ticket. The original
scalar/history entry remains alongside this new path.

resume_publish_striped now accepts the retained handoff and partial collection
under a reacquired matching queue handle. Initial attempts delegate to that same
body, so retry uses the same recheck/preparation/reservation/publication sequence.
CollectionHandoff::cancel restores all acquired leases to their actual counters,
extracts the exact controllers and reconstructs State without a pending generation;
its old-generation map is unchanged. This gives busy results both retry and
cancellation paths while preserving the borrowed transition authority.

PendingHandoff now consumes a State with a matching pending generation, lends its
actual sealed controllers to collection and retains the live generation's exact
map. Its restoration/cancellation deliberately preserves pending Some; restoring
controllers alone is not evidence of callback completion.

Handle recover_pending_striped polls this actual collection. Busy return preserves
the handoff and prepared ticket. With all pending stripes drained, it borrows the
exact DrainSet, acquires the prepared queue, uses the shared take/reset/release
operation, and recovers every allocation through that same AtomicPtr registry.
The returned heap-permission vector is matched element-for-element (in pop order)
to the protected withdrawal snapshot. Recovery now instantiates production's
finish_rotation: callback output is established first, then PendingHandoff::finish
requires complete collection plus the ready authority for that pending queue in
the same Current instance. It restores all leases, reconstructs State and clears
pending, preserving the live-generation map and returned recovered allocations.
Ordinary restore/cancel still retain pending; they do not stand for callback
completion. Moving the clear effect before the callback is rejected as use of the
collection after it has been consumed by finish.

This is not yet the complete native migration: the constructor still receives
matching maps as premises, and native constructor/lock/weak-memory correspondence,
Cache and destruction remain open. Recovered heap permissions are not
destructor-return receipts; pending completion here is retirement callback return.


### Cache observations use actual admission and drain resources

Cache atomic_admission retains the permit returned by the actual 32/64-bit counter
CAS and the matching Counter reference. Its observation adapter lends that permit
to the existing conserving ObservationLedger. Release consumes the wrapper and
returns that same permit through the counter's release CAS; a live observation
borrow prevents moving/releasing the wrapper. Rejected admission and fail-stop
produce no wrapper.

DrainLease/DrainSet now exclude borrowed admission permits as well as retained
shares. Cache's ScopedObservation uses this exclusion directly. Frozen-ledger
narrowing removes only gates covered by actual drains, and the conserving ledger
derives zero observations once its remaining coverage is drained. recover_after_drain
combines that result with the matching final-pin ticket, zero-pin token and
allocation storage to recover the exact initialized HeapPermission. These methods
accept no fabricated drain histories.

The actual Counter-to-observation borrow and actual drain-to-recovery premises are
connected. This does not yet instantiate the native Cache index or pin AtomicUsize
registry, migrate its complete queue/preparation/reclamation driver, or prove its
weak memory and Box/Drop behavior. Those mappings remain required; the new
capabilities alone do not establish full native Cache refinement.


### Cache pin atomics conserve the allocation's actual linear resources

Cache atomic_pins binds a vstd AtomicU32/AtomicU64 value to the same cache_pins
instance's count and retiring tokens. Observation-to-lease and existing-pin-to-anchor
acquisitions use the shared acquire kernel in a CAS retry loop; only successful
CAS updates count and issues a fragment. Results include a ghost sampled count
whose shared-kernel classification distinguishes Zero and Overflow. Failed CAS
retains resources and retries. ScopedObservation supplies its actual storage-backed
observation to the acquire adapter.

Release consumes a matching pin at fetch_sub. The pin-positive property excludes
underflow for valid ownership; final release creates the exact retirement ticket.
release_covered converts that ticket to RetiredNode and freezes the same node's
observation coverage. Recovery opens this same atomic invariant, derives its zero
count and retiring flag from the final ticket, uses actual drain-derived zero
observations, and withdraws the exact allocation permission from storage. Count
and retiring are no longer independent caller-supplied zero premises in this path.

These are SeqCst vstd atomic operations. The native fetch_update lowering,
Relaxed/Release updates, final Acquire fence and release sequence, native node/index
representation, complete queue-driver instantiation and Box/Drop remain separate
obligations. The adapter does not claim that the native CacheNode binary was
accepted directly by Verus.

The pin adapter constructor now consumes an initialized `HeapPermission<T>` and
creates its node instance, atomic count/retiring invariant, creator capability,
allocation token and observation ledger together. Matching identity and drain
domain are constructor postconditions, not caller-supplied matching-token premises
for this entry point. This does not verify the native allocator or node layout.

### Cache retirement payloads retain their actual resource lock

`queued_atomic` constructs a node-bound `vstd::rwlock::RwLock` over the allocation
and observation ledger returned by atomic pin initialization. Final release opens
this lock and freezes the same ledger before creating a queue payload. The payload
borrows its node; recovery opens that exact lock and derives allocation presence
from the linear retirement ticket, rather than assuming an external allocation
matches. Atomic registration uses the existing Current/queue-lock implementation.
Locked batch detachment and per-node recovery preserve every exact memory in
reverse pop order under borrowed full-domain DrainSet authority.

This is a verified adapter with trusted vstd lock semantics, not direct verification
of the native Cache node layout. Scoped observation operations, monotonic narrowed
coverage through the lock, pending-generation preparation, native ordering and
Box/Drop still need integration. The full-domain recovery entry point does not
replace the required normal two-generation reclamation path.

### Cache coverage bounds survive resource-lock reacquisition

The queued atomic adapter now stores a tokenized monotonic coverage snapshot in its
resource lock invariant, equal to the observation ledger's coverage and frozen flag.
Final release freezes that snapshot and gives the entry a matching bound receipt.
Preparing an entry narrows the actual observation ledger using DrainSet exclusions,
updates the snapshot, and issues a persistent receipt. Its guarantee survives later
lock acquisitions and additional narrowing; prepared recovery derives sufficient
drain coverage from this receipt rather than a caller-provided observation count.

Queue preparation borrows the actual queue write handle, polls real stripe collection,
leaves the queue untouched on incomplete drain, and restores collected leases only
after narrowing ends. The reservation adapter rechecks Current, prepares and reserves
while borrowing the same handle. Prepared detachment consumes the matching phase
receipt and recovers the exact batch using its specified drain domain. The complete
publication/pending callback driver and observation operations through the resource
lock remain to connect; native representation, weak memory and destruction remain
outside this increment.

### Cache publication and pending completion use the striped resource driver

Cache now invokes the actual striped rotation driver from its prepared queue
reservation. Retry retains the borrowed transition handoff and partial collection;
stale reservation restores controllers and queue ownership. Successful publication
uses the shared seal/pending/publish/reopen sequence and returns the old queue's
prepared receipt with precisely its stripe IDs.

Pending completion borrows that collection's actual DrainSet, acquires the matching
queue, recovers every exact allocation through its node lock, and invokes the shared
`finish_rotation` callback-before-clear sequence. The returned state is sealed for
the pending generation, preserves live-generation controllers and clears pending
only after recovery returns matching empty-queue readiness. Recovery means heap
permission recovery, not native destruction. Scoped observation integration into
this node, native representation and weak-memory/Box/Drop correspondence remain open.

The Cache-specific lifetime gate mutates the shared macro to clear before callback;
it must fail with E0382 for use of the consumed collection in `queued_atomic.rs`.
This is checked separately from SMT mutations and after the untouched Cache baseline.

### Cache observations retain owned admission shares in the node ledger

The queued node now uses `retained_observations::Ledger` instead of extending a
borrowed admission over the lifetime of the node. Each ledger entry owns a Share
from the existing storage-backed admission Scope and the node's linear observation
fragment. A linear receipt fixes the ledger, scope and exact memory; receipt-map
conservation prevents foreign or already-ended observations from being used.
Ending an observation consumes that receipt and returns its Share to the same Scope.
The existing actual counter `acquire_scope`/`release_scope` contracts require all
shares returned before releasing admission.

A persistent retirement history in the pin state machine excludes any live pin;
the retained ledger uses it to reject new observations after final release. Node
observe/end and observed pin acquisition hold the same actual resource lock; pin
acquisition executes the existing CAS adapter. Final release, coverage narrowing,
prepared queue rotation and recovery now operate on this retained ledger. The
borrowed ledger remains in the older proof adapters, but is no longer the queued
node's resource representation. Node/Entry no longer carry its scope lifetime.

These are resource adapters. Mapping native index lookup to the resident fragment,
returning native scoped references through this representation, native weak memory
and Box/Drop remain unproved. No extra native lock or representation change was
introduced by this proof-only migration.

The `lookup_pin` adapter now directly composes counter `acquire_scope`, node
observation, atomic lease acquisition, observation end and `release_scope`. A
successful returned pin carries the same resident memory and survives scope release.
Rejected/fail-stop admission returns no pin result. The caller still supplies the
resident fragment and proves counter membership in the node domain; this is not yet
a proof of the native index lookup, resident recheck or rollback path.

Native `ResidentEntry::clone` produces a non-owning snapshot. Consequently the new
`lookup_pin` helper's borrowed resident fragment is a stronger premise than native
`get_at_epoch` retains across lookup; it must not be presented as that whole method's
refinement. The split observe/acquire APIs permit the resident fragment to end
between index observation and pin acquisition, but native snapshot transfer,
generation/resident rechecks and rollback still require explicit correspondence.

### Cache native lookup control flow shares one macro with Verus and Loom

`get_at_epoch` now instantiates `lookup_after_observation!` from the same file as
pin arithmetic. The common expression performs eligibility rejection, pin outcome
classification, resident recheck, rollback pin release, domain capture, admission
exit and retirement handoff, or returns a lease after admission exit. Zero-pin
rejection explicitly exits admission; overflow does not return normally.

`complete_lookup` instantiates that expression with a pre-existing owned observation
receipt, actual pin atomic acquisition/release and actual counter scope release.
It does not borrow a resident pin across acquisition. Success yields the exact lease
fragment only for eligible/resident samples; rollback yields at most the exact
final-pin retirement entry, never a lease. Its overflow backend is a nonreturning
loop; native abort behavior remains the fail-stop TCB contract.

The Loom temporal-reclamation reader also uses the same macro and pin arithmetic.
Its rollback waits for admission quiescence after leaving its own scope. Verus takes
metadata samples as inputs and returns the retirement handoff instead of executing
the native enqueue/drain/destructor callback. Those effects, raw index snapshot
transfer, address mapping, weak memory and native destruction are still not an
end-to-end theorem. The shared expression removes branch-order drift for this slice.

### Resident index guard transfers residency protection into an observation

`resident_index` now models a protected entry using an actual vstd read/write lock.
The stored Resident owns the matching resident pin; lookup borrows that pin only
under the read handle, issues a retained observation and returns a Snapshot with
its own receipt. The snapshot references the node independently of the cell guard.
Removal transfers the resident pin under a write handle; retiring it does not
consume earlier snapshot observations. Snapshot completion calls the shared lookup
expression, and `lookup_complete` surrounds the transfer with actual counter
admission/release, without a caller-supplied resident pin spanning acquisition.

The locally resolved quick_cache 0.7.0 `sync::Cache::get` was inspected: its
`shard.read().get(hash, key).cloned()` expression clones while the read guard is
alive. Native ResidentEntry clones are non-owning. That identifies a candidate
observation-transfer point, but the per-entry adapter is not a verification of
Quick Cache hashing, eviction policy, native Clone/Drop instrumentation or the
mapping between library locks and this vstd lock. Those remain explicit library/
representation obligations, as do metadata loads, weak memory and destruction.

### Typed lease references borrow the exact pin until their last use

The queued node exposes a typed read through its existing storage-backed pin guard.
The new owned Lease binds node ID, Lease role, initialized HeapPermission and pointer;
read borrows that pin, and release consumes it through the same node atomic. Resident
snapshot completion and admitted cell lookup now return this Lease instead of an
unwrapped pin token. Reads return the exact stored T; no permission is cloned.

The Verus lifetime gate rejects releasing the Lease before a later use of its value
reference. A separate native cargo check probe rejects dropping CacheLease before
using its Deref result; both fail with E0505 after their clean baselines. This native
check verifies the public borrow lifetime, not the unsafe dereference implementation.
Mapping generic proof T to native CacheNode<V>, its inline value-field projection,
raw address/provenance, weak memory and Box/Drop remains to be proved.


### Inline value projection uses the shared allocation field declaration

`cache/node_layout.rs` supplies the CacheNode field declaration to native Rust and
`inline_value::Allocation<V>` in Verus. The declaration preserves native field
order, types and visibility; there is no added metadata wrapper or runtime field.
Both native lease/scope reads and the permission-backed ValueLease read use the
same `.value` projection expression. ValueLease first borrows the entire initialized
Allocation through its exact allocation pin. Its returned reference borrows the
lease, and release transfers that same whole-allocation permission if it retires.

This is a shared field/projection refinement, not a native layout/ABI theorem.
The proof instantiation uses std atomic field types but does not connect their
addresses to the separate verified atomic backend; its domain is an opaque raw
pointer instead of native NonNull<CacheLookupDomain<V>>. Allocation construction,
index/metadata correspondence, native pointer provenance, weak memory, and Box/Drop
remain obligations. In particular this wrapper does not manufacture a native
allocation or establish that the native pin atomic governs this permission.


### Native reclamation consumes the retirement entry

`reclaim_cache_node` now takes ReclaimEntry<V> by value instead of accepting its
copyable raw pointer. Both the certified-batch loop and never-published fast path
move the entry into this boundary. Only there is the pointer extracted for
Box::from_raw. The inline value is still destroyed in place; the existing panic
containment and Box deallocation path are preserved.

A native compile-fail probe rejects passing the same entry twice with E0382. The
corresponding verifier probe rejects a second Entry::recover call on the same
retirement resource. These are linear consumption checks, not a proof that a raw
pointer cannot be forged by unsafe code. Quiescence remains an unsafe precondition
of the native single-entry boundary; its final-pin origin, exact allocation/proof
resource mapping and Box/Drop contract are not discharged by this API change.


### Native Cache batch ownership retains domain lifetime

ReclaimEntries<'domain, V> borrows CacheLookupDomain, including empty results.
Drained/closed certificate callbacks attach the owner; nonempty merges check owner
address equality. Native compilation rejects destroying that owner before consuming
the batch. This connects native lifetime custody to the explicit ownership shape
used by the resource adapters, but does not establish equality of native addresses
and proof instance IDs, queue payload provenance, or Box/Drop semantics.


### Cache locked recovery requires payload owner agreement

Both recover_prepared_locked and recover_locked now require every admitted queue
payload to have the same owner as the queue being recovered. Their postconditions
establish valid_records for the original queue contents as well as exact recovered
memory correspondence. Previously the lower APIs required valid entries and drain
coverage but did not retain the owner constraint imposed by the higher striped
caller. The actual queue constructor and striped path already supply that condition;
this removes the weaker generic recovery contract. It is an ownership contract
strengthening, not a proof of native address-to-instance representation.


### Cache and Handle share the checked batch-transfer expression

The existing append_owned_batch macro moved from handle/domain/protocol.rs to
retirement_queue.rs. Both native batch implementations and the generic Verus
Handle Batch::append adapter now instantiate it; Cache also uses it to validate
the first returned batch against the requested owner. There is no compatibility
copy of the former macro. Address comparison precedes the supplied operation.

The generic proof establishes rejection without payload changes for different
owner addresses, and exact sequence concatenation with an emptied source on
acceptance. The macro now returns the operation's result, allowing Cache's final
owner validation to return the batch directly. Native SmallVec versus proof Vec,
owner address/provenance mapping, and destructor effects remain backend
obligations; this is shared control-flow verification, not full native ownership
representation refinement.


### Native Cache registration checks the retired node's captured domain

ReclaimEntry now stores named pointer, weight and domain fields. The only production
constructor is final-pin release, which captures the domain directly from the live
node alongside its allocation pointer and weight. Before generation selection or
queue locking, enqueue_reclaim_impl compares that captured domain with self using
the shared checked-owner expression. A mismatch fails stop before registration;
there is no separate unchecked enqueue helper. Sentinel tests explicitly name
their synthetic owner instead of supplying ownerless entries.

This enforces the native counterpart of the owner-equality precondition already
required by the Cache registration proof. It does not prove that native pointers
are the same ghost identities, establish all allocation constructor/provenance
obligations, or discharge weak-memory/Box/Drop refinement. The retirement entry
stores one additional pointer; it is cold queue metadata, not a node or lease field.


### Inline allocation owner is obtained by a permission-backed field read

inline_value::initialize now borrows the initialized HeapPermission with ptr_ref,
reads its domain through node_layout::domain!, then initializes the pin ledger
using that value. The native final-pin retirement constructor uses the same field
projection. The proof initializer returns the exact creator permission and a node
whose owner equals the stored allocation domain; it takes no independent owner
argument. ValueLease now requires that agreement, and its release guarantees any
retirement entry retains the allocation's stored owner.

This connects the inline allocation field to the verified owner at this entry
point. Generic Node construction still exists for other resource adapters; the
native Box constructor, real atomic-field-to-backend mapping, opaque raw domain
versus NonNull<CacheLookupDomain<V>>, and generation/index wiring are not thereby
proved. The initializer consumes an already-initialized permission rather than
proving Box allocation itself. Full native refinement remains open.


### NonNull representation remains an explicit unsupported-library boundary

A direct compile probe against the installed Verus rejects both
std::ptr::NonNull<u8> and NonNull::as_ptr as unsupported. The diagnostic suggests
external_type_specification and assume_specification; neither was added. The inline
allocation adapter therefore still uses an opaque raw domain pointer. A passing
raw-pointer field proof is not a proof of the native NonNull representation.

The TCB audit now rejects project-local assume_specification and axiom fn in
addition to assume calls, including tokens separated by newlines/comments. External
type/function specifications require the same explicit allowlist treatment as
external bodies. This closes ways that unsupported-library adapters could otherwise
silently introduce trusted facts while still reporting a zero-assume audit. The
existing library TCB remains explicit; the project external allowlist is still empty.


### Protected index transports allocation representation facts into lookup

resident_index's lock predicate now includes a ghost memory predicate. new_checked
establishes it on the stored resident permission; guarded lookup transfers it to
the Snapshot, and lookup_complete preserves it for the exact observed permission.
The ordinary generic constructor uses the always-true predicate.

inline_value::new_index instantiates the protected predicate with equality between
the allocation's stored domain and the index owner. Its lookup adapter runs actual
counter admission, guarded observation transfer and the shared lookup transition;
it constructs ValueLease from the resulting pin and preserves the stored owner in
any rollback retirement entry. No extra per-lookup assertion of allocation/owner
agreement is supplied after the index access. That fact now comes from the lock
invariant established at checked index construction.

This connects the typed allocation representation to the existing executable index
adapter. It does not verify the native Quick Cache implementation or native
NonNull/atomic-address mapping, metadata sampling, allocation/Box/Drop, or weak
memory. Checked index construction still requires a valid resident pin and the
allocation owner relation; issuance from native allocation remains a separate
boundary, while inline initialize supplies the relation in the permission model.


### Typed lookup derives eligibility from observed allocation generation

The typed inline lookup no longer accepts an arbitrary eligible flag. Snapshot
reads generation through its retained observation receipt: the queued node borrows
the matching storage-backed observation under its resource lock, obtains ptr_ref
permission, copies the immutable generation, and releases the lock. The returned
scalar is proved equal to that exact allocation's generation.

Native get_at_epoch and typed proof lookup now instantiate the same generation
field projection and epoch/residency comparison expression in node_layout.rs.
Successful ValueLease therefore has the requested epoch; a mismatched observed
epoch cannot produce either a lease or a rollback retirement entry. The existing
shared completion expression still controls pin acquisition and observation exit.

Resident-before and resident-after remain sampled boolean inputs in the proof.
This increment does not connect native AtomicBool loads or memory order, and the
resource lock is the proof adapter rather than a newly introduced native lock.
Native Quick Cache, NonNull, atomic-address, Box/Drop and full driver correspondence
remain explicit obligations.


### Typed lookup performs actual resident loads at the shared control points

A compile probe confirms the installed verifier accepts std AtomicBool::load with
Ordering::Acquire (unlike the recorded NonNull boundary). The typed lookup no
longer receives resident-before/after booleans. Its Snapshot reads the embedded
Allocation.resident under the retained observation's actual PointsTo permission.
The first read occurs only after matching the requested epoch; the recheck is
inside lookup_after_observation, after successful pin acquisition and before
observation discharge. Native get_at_epoch and the Loom reader use the same
node_layout::resident load expression, including Acquire ordering.

The typed adapter records the first sampled result in ghost output. A successful
lease requires a true first sample; a false sample yields no lease or retirement.
complete_inline_lookup records the recheck sample and distinguishes successful
lease from rollback retirement. The earlier generic boolean-sample helper remains
for generic protocol adapters; the typed entry now calls the actual-load path.

This establishes permission-backed access and shared control placement, not a
complete atomic-history theorem. It does not bind concurrent residency stores to
the index model, identify the native pin field with the separate vstd pin atomic,
or prove weak-memory publication/release sequences, native NonNull/index/Box/Drop.
The standard atomic primitive semantics remain library TCB.


### Direct std atomic calls do not provide the value-history contract

The installed Verus accepts a direct AtomicBool Acquire load, but fails the true
single-threaded assertions that a newly created AtomicBool(false) loads false and
an AtomicUsize(1) loads 1. The same initial-value assertion succeeds for
vstd::atomic::PAtomicBool with its linear permission. These are specification
coverage probes, not native atomic failures. Reproduce with
`python3 -B tools/probe_verus_atomic_contracts.py`; it reports proved/unproved
separately and rejects compiler/tool errors instead of counting them as evidence.

Thus the resident-load adapter establishes field access and placement but cannot
claim initialized residency or coherent store history from std calls alone.
The matching [vstd primitive source](https://raw.githubusercontent.com/verus-lang/verus/671956e/source/vstd/atomic.rs)
owns a private native atomic and pairs it with a permission; its operations use
SeqCst. The [ghost wrapper source](https://raw.githubusercontent.com/verus-lang/verus/671956e/source/vstd/atomic_ghost.rs)
constructs this primitive and associates the permission with an invariant. These
constructors create an atomic rather than importing an existing native field.
A separate newly-created proof atomic is not evidence for the existing field.
Connecting that field to its permission and preserving native ordering remains
required; no cast, ownership axiom or TCB exception was added to bridge it.


### Shared Cache pin CAS retry control

Native CacheNode, the Loom temporal-reclamation reader, and both Verus pin
acquisition methods now instantiate `pin_transitions::acquire_retry!`.
It loads once, classifies zero/overflow, attempts a weak CAS, returns a pin only
on success, and reuses the observed value on failure. The native implementation
retains Relaxed load/failure and the caller's Relaxed or Release success ordering.
The Verus backend retains vstd's SeqCst primitive; its invariant reconnects each
successful CAS with the same instance's linear pin token. An anchored acquisition
also proves the failed-CAS observation remains positive, including spurious failure.

This shares executable retry control, not the atomic field or its weak-memory
contract. There is no termination/fairness claim for a contended or spuriously
failing CAS loop. Allocation identity, native atomic permission adoption, and
Box/Drop refinement remain open. No assumption or external specification was added.


### Shared final Cache pin release tail and fence boundary

`pin_transitions::release_pin!` now shares decrement-before-classification and
last-pin-only fence-before-retirement control between CacheNode, both roles of
the Loom temporal-reclamation model, the focused Loom final-holder test, and
both Verus atomic pin widths. The latter consumes exactly the same instance's
pin token and returns a retirement token iff the observed decrement is final.

Native/Loom supply a Release fetch_sub and an Acquire fence. Verus uses vstd's
SeqCst fetch_sub and an empty fence expression: the installed verifier rejects
the direct std fence call as unsupported. No assume_specification was added.
Consequently this proves the token/branch correspondence, not weak-memory fence
semantics. `just cache-release-ordering` supplies separate executable evidence:
the unchanged focused Loom test passes, while removing the shared fence or
placing it after retirement fails a stale-value assertion. Parser/compiler
errors and an absent/filtered test do not count as a successful mutation gate.
The gate copies sources to a temporary workspace and is wired into CI after
Verus. CI execution itself has not been performed locally.


### Native Box recovery support boundary

`tools/probe_verus_heap_contracts.py` distinguishes proved, unproved and unsupported
outcomes using the installed verifier. With 0.2026.09.13.671956e, Box::new's value
and a PointsTo-guarded pointer borrow verify. Box::into_raw, Box::from_raw and the
Box::leak-to-pointer expression used by Cache are unsupported. Compiler rejection
is reported as unsupported, never as successful verification; unexpected compiler
errors fail the diagnostic. This is an informational probe, not a gate requiring
future Verus versions to retain those limitations.

Thus recovering HeapPermission from the retirement ledger still does not establish
the native Box handoff. Adding a local trusted specification for that handoff would
move it into the TCB rather than prove it; no such specification has been added.
Likewise, replacing in-place Box destruction with ptr_read would change production
behavior for large inline values and is not an acceptable proof-only substitute.
The native inline-payload test now verifies that Drop executes at the exact address
previously exposed by the lease, in addition to alignment and exactly-once drop.


### Reader-owned retained Cache observations

Retained Ticket now owns its Cache observation token as well as its receipt. A
type invariant relates their exact HeapPermission. Node identity is an explicit
precondition threaded through Node and Snapshot; completion consumes the matching
node observation and receipt together. The ledger retains admission shares and
count/coverage bookkeeping, not the reader's Cache observation fragment.

Pin CAS and immutable-generation/resident loads borrow directly from Ticket and
no longer execute a ledger lock operation. Registration, completion, final-pin
freezing and recovery still use the proof ledger lock, so native synchronization
refinement is not complete. The separate vstd pin atomic also remains separate
from the native allocation's atomic field. No trusted adapter or assumption was
introduced by this resource redistribution.


### Pin release before proof ledger locking

Node::release now performs the pin atomic decrement before acquiring the ledger
lock. A nonfinal decrement returns immediately. Only a final retirement token
allows construction of the RetiredNode and subsequent observation/coverage
freezing under that lock. The earlier release_owned_covered helper was removed;
its lock-spanning release is no longer the queued node's executable behavior.

The interval between final decrement and ledger freezing cannot issue a new
observation from a live resident pin: the pin resource machine excludes a live
pin once final retirement is issued. Existing reader-owned Tickets continue to
retain observations until their own completion. This uses existing resource
contracts, not a new trusted specification. Final completion, observation
registration/completion and recovery still have proof-only synchronization.
