# Trusted Computing Base (TCB) for Verus Formal Verification in xlfn

## 1. Overview & Separation of Verification Responsibilities

xlfn employs a multi-tiered verification architecture to ensure safe, high-throughput concurrent computation, caching, and generational rotation.

| Layer | Target Scope | Verification Engine | Primary Guarantees |
| :--- | :--- | :--- | :--- |
| **Protocol** | Whole-system & lifecycle | **Lean 4** | Generational transition safety, shutdown quiescence, quiescence certificates |
| **Implementation** | Concurrent ownership & state machines | **Verus** | Bit representations, atomic transitions, resource invariants, linear raw pointer ownership |
| **Memory Order** | Memory-ordering regressions | **Loom** | Exhaustive thread interleavings of Acquire, Release, AcqRel, and Relaxed orderings |
| **Runtime UB** | Aliasing & undefined behavior | **Miri** | Stacked Borrows / Tree Borrows, absence of aliasing violations or memory leaks |

Formal verification with Verus mathematically proves the logical consistency and invariant preservation of the Rust implementation. It does not verify the physical execution hardware or the host operating system. Consequently, the assumptions and prerequisites at the verification boundary (the Trusted Computing Base, or TCB) are explicitly defined and strictly audited.

---

## 2. Trusted Computing Base (TCB)

The following components and properties are treated as **Trusted Assumptions (TCB)** outside the scope of Verus automated proofs.

### 2.1. External Libraries & Runtime
1. **`parking_lot::Mutex` Semantics**:
   - Mutual exclusion: `lock()` grants critical section access to at most one thread at a time.
   - Lock release: `unlock()` / `drop()` correctly yields access to pending or subsequent waiters.
2. **`parking_lot::Condvar` Semantics**:
   - `wait()` atomically releases the associated mutex and enters a blocked wait state, reacquiring the mutex upon wakeup.
   - `notify_all()` and `notify_one()` reliably unblock waiting threads without lost wakeups.
3. **Rust `std::sync::atomic` Machine Semantics**:
   - Atomic read-modify-write and memory operations (`compare_exchange`, `fetch_add`, `fetch_or`, `load`, `store`) satisfy their declared indivisibility and atomic memory semantics.
4. **Allocator & Pointer Layout**:
   - `Box::new` / `std::alloc` allocates a valid, uniquely owned memory block of appropriate alignment and size.
   - `NonNull::new_unchecked` is sound when invoked on non-null pointer values.
5. **Compiler & SMT Solver Infrastructure**:
   - The `rustc` compiler generates sound machine code faithful to Rust operational semantics.
   - The Verus frontend verification pipeline and the backend Z3 SMT solver are sound.

---

## 3. Proved Properties

The following properties and theorems are statically proved within Verus with zero verification failures and zero unverified assumptions:

### 3.1. SealableCounter
- **`[SC-1]` Bounded Active**: The `active` reader count never exceeds `ACTIVE_COUNT_MASK`.
- **`[SC-2]` Acquire Correctness**: Successful acquisition strictly increments `active` by $+1$, preserving `sealed` and `waiting` flags.
- **`[SC-3]` Sealed Rejection**: If `sealed` is set, `acquire` unconditionally returns `None`.
- **`[SC-4]` Release Precondition**: Release cannot succeed if `active == 0` (absence of counter underflow).
- **`[SC-5]` Release Correctness**: Successful release strictly decrements `active` by $-1$, preserving all status flags.
- **`[SC-6]` BecameIdle Equivalence**: `BecameIdle` is returned if and only if previous `active == 1`.
- **`[SC-7]` Reopen Precondition**: Reopening succeeds only when `sealed && active == 0`.
- **`[SC-8]` Reopen Postcondition**: Upon successful reopen, `!sealed && !waiting && active == 0` holds.
- **`[SC-9]` Retain Final Capability**: When `waiting && active == 1`, `release_without_notification` returns `None`, forbidding the last permit from disappearing before the waiter notification mutex is acquired.

### 3.2. DrainGate (Phase 3)
- **`[DG-1]` Permit Liveness**: The existence of an active permit strictly implies `active > 0`.
- **`[DG-2]` No Admission After Seal**: Once sealed, no new permits can ever be acquired.
- **`[DG-3]` Quiescence On Drain**: Upon completion of `seal_and_wait`, `sealed && active == 0 && permits == 0`.
- **`[DG-4]` Reclamation Precondition**: Memory reclamation capability for protected resources is obtainable only after `active == 0`.
- **`[DG-5]` Mutual Exclusion with Final Release**: The owner cannot be deallocated while the final release is in progress (coordinated with `SC-9`).

### 3.3. PublishedOwner (Phase 4)
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

### 3.6. ServiceSlot (Phase 7)
- **`[SS-1]` Single Service Ownership**: Sole ownership of `PublishedOwner` / `Box<R>` is strictly conserved across `Ready`, `SealingTxn`, `TeardownFaulted`, and `ServiceSeal::Present`.
- **`[SS-2]` Publication Validity**: The fast-path published pointer is non-null if and only if the slot is `Ready` and readers are open.
- **`[SS-3]` Withdraw Before Drain**: During `seal()`, the published pointer is nullified and readers are sealed prior to waiting for reader drain.
- **`[SS-4]` Quiescence Precondition for Box Extraction**: `into_box` is invoked only after `readers.active() == 0`, ensuring race-free extraction.
- **`[SS-5]` Fault Isolation**: Any failure during initialization or teardown transitions the slot to a fault state (`InitFaulted` or `TeardownFaulted`), preventing invalid state reuse.

### 3.7. CacheLease & Temporal Reclamation (Phase 8)
- **`[TR-OBSERVE-1]` Pointer Observation Soundness**: Observing a raw cache node pointer implies the object is live (`Published` or `Retired`), never `Reclaimed`.
- **`[TR-LEASE-1]` Lease Pin Safety**: An active `CacheLease` pin strictly prevents node reclamation, guaranteeing safe shared dereference.
- **`[TR-ADMISSION-1]` Admission Boundedness**: Pointer observations can only occur under the protection of an active admission domain permit (`observing <= admissions`).
- **`[TR-RECLAIM-1]` Drained Reclamation Precondition**: Transitioning a cache node to `Reclaimed` requires all admissions, observers, and pins to be zero (`admissions == 0 && observing == 0 && pins == 0`).
- **`[TR-NO-UAF]` Fundamental Temporal Safety**: In any valid system state, holding any capability (pin or observation) guarantees `status != Reclaimed`.

### 3.8. HandleReadDomain & Publication (Phase 9)
- **`[HD-1]` Admission Isolation**: Handle lookup operations proceed strictly under active `HandleDomainPermit` protection; closed domain unconditionally rejects new readers.
- **`[HD-2]` Binding Retirement Before Reclamation**: Withdrawing an active binding moves it to the generation-bound pending queue and tracks it in `debt`, forbidding immediate destruction.
- **`[HD-3]` Generational Quiescence**: Draining a retired generation strictly requires zero active readers in that generation.
- **`[HD-4]` Linear Binding Deallocation**: Each retired `BindingRecord` in the pending queue is consumed and dropped exactly once.
- **`[HD-5]` Destruction Barrier**: Complete seal and domain teardown establishes zero active readers, zero queued records, and zero outstanding debt (`debt == 0`).

---

## 4. TCB Governance Rules

1. **Zero-Assume Policy**:
   - In both production code and Verus proofs, `assume(...)` directives are strictly prohibited (enforced to be 0 by CI).
   - Any temporary assumption requires an explicit justification, an issue reference, an expiration deadline, and approval in the TCB allowlist.
2. **`external_body` Allowlist Management**:
   - Any functions annotated with `#[verifier::external_body]` must be explicitly registered in `tools/verus_external_allowlist.json`.
3. **Continuous Integration Gates**:
   - `just verus`: Automated execution of all Verus proof crates, requiring 0 verification errors.
   - `just verus-audit`: Automated inspection ensuring 0 `assume` directives and 0 unapproved `external_body` items.
