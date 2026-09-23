# Verus Formal Verification in xlfn

This directory contains formal verification artifacts, specifications, and proofs powered by [Verus](https://verus-lang.github.io/verus/), a tool for verifying the formal correctness of Rust code using SMT (Z3).

The vstd idle-publication path now also runs verified Cache and Handle recovery
callbacks before restoring sealed controller state, using the production-shared
`finish_rotation!` ordering. The callbacks return the exact withdrawn queue's
heap permissions and matching ready token. Native field identity, callback
callsite correspondence, `Box::from_raw`/`Drop`, and weak-memory ordering remain
unproved; this does not change the no-new-trusted-adapter policy.

Production Cache and Handle retirement now hold their read domain and both
retirement queues in one `RotatingRetirementDomain`. Its registration, rotation
barrier and certified take select fields of that owner, removing a caller-chosen
queue-array identity at those sites. The vstd queue and lock instances are still
separate objects; their correspondence to these native fields is not proved.

---

## 1. Architectural Role & Responsibilities

xlfn enforces temporal ownership and reclamation guarantees through a multi-tier verification stack:

$$\boxed{\text{Lean 4 = Protocol Correctness}}$$
$$\boxed{\text{Verus = Implementation-Oriented Protocol Verification}}$$
$$\boxed{\text{Loom = Memory-Ordering Schedule Checks}}$$
$$\boxed{\text{Miri = Residual UB / Stacked & Tree Borrows}}$$

| Tool                                             | Target Scope                       | Key Verification Goals                                                       |
| :----------------------------------------------- | :--------------------------------- | :--------------------------------------------------------------------------- |
| **Lean 4** (`formal/XlFnFormal/`)                | Protocol & Lifecycle transitions   | Whole-system quiescence, shutdown certificate, generation invariants         |
| **Verus** (`verification/verus/`, `xlfn-kernel`) | Concurrency protocol & models      | Dual 32/64-bit arithmetic, state transitions, tracked tokens, raw pointers   |
| **Loom** (`cargo test -p xlfn-kernel loom_`)                   | C++11 memory-order regressions     | Exhaustive or explicitly bounded interleavings, depending on the test                   |
| **Miri** (`just miri`)                           | Operational semantics              | Stacked Borrows / Tree Borrows, no aliasing violations or memory leaks       |

---

## 2. Invariant ID Traceability & Refinement Architecture

Lean 4 theorems, Verus verified properties, and ordinary Rust safety comments share unified traceability tags:

```
                  [TR-RECLAIM-1]
                 /      |       \
                /       |        \
         Lean 4       Verus        Rust
     (Safety.lean) (kernel proof) (SAFETY: ...)
```

The verification stack combines proofs and runtime checks across distinct boundaries. Only the shared transition and control expressions have direct source correspondence:

```text
┌────────────────────────────────────────────────────────┐
│ Lean 4 (`formal/XlFnFormal/`)                         │
│ - Protocol correctness, generational rotation, drain   │
└───────────────────────────┬────────────────────────────┘
                            │ Shared Invariant IDs ([SC-*], [DG-*], [TR-*], etc.)
                            ▼
┌────────────────────────────────────────────────────────┐
│ Verus (`verification/verus/`)                          │
│ - Implementation-oriented protocol models & bit proofs │
│ - Dual 32-bit (i686) & 64-bit mathematical safety      │
│ - SSOT direct transitions verification                 │
└───────────────────────────┬────────────────────────────┘
                            │ Shared transition/control expressions (subset only)
                            ▼
┌────────────────────────────────────────────────────────┐
│ Production Rust (`crates/xlfn-kernel/`, `crates/xlfn`) │
│ - Verified pure transition kernels                     │
│ - Atomic RMW loops, OS synchronization, allocations   │
└───────────────────────────┬────────────────────────────┘
               ┌────────────┴────────────┐
               ▼                         ▼
┌──────────────────────────┐ ┌──────────────────────────┐
│ Loom (`cargo test -p xlfn-kernel loom_`)│ │ Miri (`just miri`)       │
│ - C++11 memory orderings │ │ - Pointer provenance     │
│ - Bounded/exhaustive tests  │ │ - Aliasing / Tree Borrows│
└──────────────────────────┘ └──────────────────────────┘
```

---

## 3. Current Verification Metrics & Results

| Crate / Target             | Verified Proofs  | Errors       | Assumes       | Refinement Status & Guarantees                                    |
| :------------------------- | :--------------- | :----------- | :------------ | :---------------------------------------------------------------- |
| **`sealable_counter`**     | Verified (dual)  | 0            | 0             | **SSOT transition-kernel refinement**: SC-1..9b verified on shared code (32 & 64)   |
| **`drain_gate`**           | Verified (dual)  | 0            | 0             | **Shared control-flow refinement**: wait/notification sequencing under modeled backends; permit-model bridges (32 & 64) |
| **`published_owner`**      | Verified         | 0            | 0             | **Snapshot model + tracked heap ownership**: native Box/Drop adapter remains open |
| **`operation_gate`**       | 9                | 0            | 0             | **Protocol Model**: OG-1..5 admission gate soundness, quiescence  |
| **`rotating_read_domain`** | Verified (dual)  | 0            | 0             | **Shared control-flow refinement**: modeled all-stripe publication, pending/idle recovery and barrier order; native field identity open |
| **`service_slot`**         | 15               | 0            | 0             | **Protocol Model**: SS-1..5 lazy publication, Box extraction      |
| **`cache_lease`**          | Verified (dual)  | 0            | 0             | **Shared pin kernel + resource model**: modeled idle recovery returns matching heap permission; native pin/Box identity open |
| **`handle_domain`**        | Verified (dual)  | 0            | 0             | **Protocol model + shared completion tail**: destruction-in-flight debt and linear return receipts; native binding ownership open |
| **Total**                  | **Verified**     | **0 errors** | **0 assumes** | **Shared SC/DG/RRD control and modeled Cache/Handle recovery; native connections tracked separately**|

---

## 4. Refinement Status & Roadmap

In the proof descriptions below, “actual atomic”, “real stripe collection”, and
“queue handle” refer to executable Verus/vstd backends. They do not identify
the production `std` atomic fields or `parking_lot` objects. Native identity and
weak-memory correspondence remain open unless explicitly stated otherwise.

1. **Tier 1 — Single Source of Truth (SSOT)**:
   - `SealableCounter`: The production transition kernel (`crates/xlfn-kernel/src/sealable_counter/transitions.rs`) is directly verified by Verus for both 32-bit (`i686`) and 64-bit platforms with identical fail-stop semantics (`FailStop` on overflow/underflow). The same macro-defined transition expressions are instantiated into ordinary Rust and Verus functions; this is not a proof of the compiled binary or atomic RMW implementation.
   - `DrainGate`: Production and Loom call the same `release_tail!` and `wait_loop!` macros imported by Verus from `crates/xlfn-kernel/src/drain_gate/protocol.rs`. Executable 32/64-bit backend models verify lock-before-release, notify-before-unlock, no backend access after unlock, and registration/reobservation after every wake. The wait returns only on a zero observation; it never sets active counts or permits to zero. Counter acquire/release/reopen and waiter registration are related to the width-independent permit model. An actual SeqCst AtomicU32/AtomicU64 backend now connects shared acquire/release transitions to linear permits at successful CAS operations; failed CAS retries preserve resources. Retained scopes defer release until all observation shares return. Its seal/reopen adapter now connects the real sealed bit to exclusive lifecycle control, uses the shared sealed-zero reopen transition at CAS, and proves a borrowed sealed controller rejects admission through the same acquire loop. A real sealed-zero load now issues a DrainLease by storing the zero active token and controller; restoring it consumes the lease and returns sealed control. Its borrowed zero authority excludes a matching live observation and prevents early restoration. An executable atomic_stripes collection now uses the production stripe-scan expression, retains busy controllers and completed leases across retries, exposes drain authority only when every stripe is ready, and restores exactly the matching controllers. Its invariant excludes unrelated controllers and leases; the borrowed DrainSet equals the vector gate set and restoration leaves no drain leases behind. Production, Loom and the actual-counter proof also share unconditional all-stripe sealing with matching exclusive controllers, and share the ordered reopen scan; it preserves every controller, opens a prefix and stops at the first rejection. The 32/64-bit resource-backed counter now transfers its matching zero-count token and lifecycle controller into a DrainLease at successful idle-seal CAS; the shared all-stripe loop retains exact leases and rolls them back on failure without extra executable bookkeeping. A vstd transition WriteHandle now holds those leases through all-stripe idle seal, reserved publication, and prepared-queue detachment; the detached state still owns the full DrainSet and ready ticket. Callback completion, native array/lock identity, notification and weak memory remain open.
   - The DrainGate proof is conditional on backend semantics: the model does not verify `parking_lot`, the atomic adapter, closure wiring, Rust guard destruction, or raw-pointer lifetimes. Arbitrary wake samples include spurious wakes and reopen; termination/fairness is deliberately not proved. Stable reclamation additionally requires sealing and excluding concurrent reopen. Shared invariant IDs connect Lean and Verus by traceability, not by a mechanically checked cross-prover theorem.
   - `RotatingReadDomain`: Shared begin/publication/reopen/barrier-release/finish and polled-idle control flow is verified in 32/64-bit executable models. The actual counter acquire transition excludes a delayed noncurrent selection under the generation invariant. Close no longer fabricates idle in the abstract model. Borrowed/owned production entry and Loom instantiate the verified reader retry loop; its counter-backed model allows invariant-preserving interference before each acquire. The rotation/reader/registration/permit/certificate/detachment/lock negative-mutation suite passes, including idle handoff and exact prepared-queue bound checks. An actual vstd queue WriteHandle now supports a borrowed publication-barrier certificate, with matching old-generation/domain checks. Shared early barrier release is separately rejected by borrow checking; the native publication helper also borrows its guard through the callback. The idle-only begin path now shares the success-gated seal, pending and publication expression. Its scalar Verus backend uses DrainGate’s shared load/CAS seal contract with arbitrary stale samples and preserves rotation state on failure. The resource-backed idle path now holds the vstd transition handle through all-stripe seal, reserved publication, and prepared-queue detachment, retaining the old generation DrainSet throughout. This is partial: actual callback completion, native stripe-array/CAS history, native lock/queue identity, and subsystem ownership remain open in [the completion audit](REFINEMENT_WORKLIST.md).
   - `CacheLease`: `cache/pin_transitions.rs` supplies shared 32/64-bit arithmetic for all ordinary and anchor pin acquisitions and final-release classification. Zero is terminal, overflow is distinct from zero rejection, and release underflow is fail-stop. Three arithmetic mutations are rejected. Production `get_at_epoch`, Loom and Verus now also instantiate `lookup_after_observation!`: eligibility, pin acquisition, resident recheck, rollback, admission exit and retirement handoff share their branch expression. Three branch mutations are rejected. The proof consumes an existing observation receipt and accepts arbitrary eligibility/resident samples; index snapshot transfer, metadata-load correspondence and actual rollback reclamation callbacks remain separate obligations. The separate temporal model is not yet an end-to-end proof: A storage-backed, instance-bound ownership proof now accounts for creator/resident/flight/lease pins and observation-to-pin transfer, and guards typed pointer reads with `PointsTo<T>`. Its stored `HeapPermission<T>` and final-pin retirement ticket also preserve the matching allocator deallocation capability. Cache imports the rotation resource proof and directly excludes scoped observations of a drained matching generation. Typed final-pin retirement fragments are preserved through the shared registration loop and consumed during memory recovery with matching zero-observation ledgers. An opaque ledger conserves all borrowed-permit observations; sealed-zero histories covering the node domain derive zero observations for memory recovery. Final-pin release freezes the ledger; narrowing before shared publication and pending-generation drain also derive zero observations while the reopened generation may remain active. A new 32/64-bit atomic admission wrapper retains the permit issued by actual counter CAS, lends it to the Cache observation ledger and returns it to the same counter at release. Actual DrainSet leases now exclude matching borrowed permits, derive zero observations, narrow frozen coverage and recover the exact retired allocation without caller-supplied histories. A storage-backed 32/64-bit atomic pin adapter now issues lease/anchor fragments only at successful CAS, consumes them at fetch_sub, creates final-pin retirement entries and freezes coverage. Its recovery opens the same count/retiring invariant and combines the retirement ticket with actual drain-derived zero observations. Its constructor consumes one initialized heap permission and creates the atomic, creator pin, allocation token and observation ledger with the same fresh node identity and supplied drain domain. The queued atomic adapter keeps allocation and observation resources in an erased ghost invariant, creates retirement payloads at final release, uses the atomic registration driver, and recovers each detached batch allocation under full-domain drain authority. Persistent coverage receipts bind narrowed observation coverage across later ghost-invariant openings. Queue preparation polls actual stripe collection while retaining the queue barrier, rechecks Current and reserves publication with that exact bound; prepared batch recovery requires matching drain authority. The Cache driver now connects preparation/reservation to shared seal/publish/reopen and completes pending collection, exact batch recovery and shared callback-before-clear ordering. The ghost-ledger node stores owned admission shares for observations, issues matching ledger/scope/memory receipts, returns shares on observation end and acquires actual atomic lease pins from a live receipt. Its queue driver uses this retained ledger without borrowing admission for the node lifetime. `lookup_pin` composes actual counter admission, retained observation, atomic pin acquisition, observation completion and actual admission release. A guarded resident-cell adapter now issues the observation while holding a vstd read handle, releases that handle and passes the independent snapshot into the production-shared lookup expression. This removes the need to borrow a resident pin across acquisition in that adapter; native Quick Cache representation and hash-policy correspondence remain open. Its successful lookup now returns an owned Lease whose typed read borrows the same storage-backed pin, and whose release returns that pin through the node atomic. Both the Verus adapter and native CacheLease reject release/drop while a value reference remains live; native payload-field mapping remains separate. Native Cache index/representation, typed scoped-read integration, weak memory and destruction remain unverified. Native queue cutoff, stripe identity, pin-atomic ordering and ledger event correspondence remain open. The Cache ownership/domain/composition mutation suite rejects actual-admission/drain, atomic-pin and idle-queue recovery regressions. The vstd idle-publication path now passes its exact prepared queue records and retained DrainSet into Cache heap-permission recovery. Connecting that path to native domain permits, index operations and generation-bound destruction remains open. The allocation field declaration and inline value projection now share source with native CacheNode; ValueLease borrows the whole permission-backed allocation before projecting the value. This does not establish native atomic-address/domain correspondence or Box/Drop refinement.
   - The rotation resource backend now acquires a verified library write handle, borrows it in the drained certificate, detaches typed Cache retirement entries, and consumes the handle on release. A separate borrow-check regression rejects releasing the state or handle while the certificate remains in use. This backend protects the complete model state; connecting it to native transition guards and independently changing reader atomics remains open.
2. **Tier 2 — Protocol Model Verification (Active Roadmap)**:
   - `HandleDomain`: Debt includes detached batches until destruction completes. Production and Verus share destruction/discharge/lock/notify/unlock ordering, with nonduplicable, batch-instance-bound destructor-return receipts in the executable model. Arbitrary Drop/Box behavior, the actual atomic debt adapter and binding/topic ownership remain open. Debt and queued updates now use shared checked 32/64-bit expressions, including fail-stop overflow/underflow, with conditional enqueue/detach count bridges. Native drained batches now borrow their owning HandleReadDomain, complete debt on Drop, and reject merging different owners before moving payloads. The merge expression is also shared with Verus. The Handle proof now imports the same rotation certificate/registration modules and carries initialized LinearOwner payloads through registration, certified ordinary/terminal batches and an owner-borrowing completion wrapper. Native address, reader and Drop adapters remain open. Native reader witnesses now borrow CallScope and are retained by BindingReadLease; shared append-only permit insertion preserves real linear admission tokens in Verus. The RefCell/domain-address/atomic-ledger representation remains open. Scoped binding reads also share authorization/load/borrow/identity/Live ordering with a backend that dereferences the loaded pointer using real heap permission. The reader now uses a storage-backed observation retaining its gate permit and remaining valid across publication-to-retirement transfer; an exhaustive borrowed-permit ledger derives its zero-observation count from matching drain histories or narrowed pending-generation coverage before recovery. A reusable vstd AtomicPtr backend issues observation permission at load, preserves old readers across CAS retirement and republication, and keeps per-allocation counters. Publication now shares the native empty-check/store expression, with exclusive writer authority supplied by a verified library write lock. Resource recovery uses that same atomic counter registry. An owned DrainSet of actual sealed-zero leases now derives zero observations in that registry and recovers the exact retired allocation; it also supports persistent-bound narrowing and pending-only recovery while another gate's reader stays live. The new prepare_collected_records adapter polls actual stripes, prepares every record using a borrowed complete DrainSet, then restores controllers; a busy result preserves the records and partial collection for retry. A protected queue adapter derives payload premises from the matching library lock and preserves its phase and queue identity while borrowing the WriteHandle. A further adapter composes actual current recheck and atomic reservation while preserving partial collection or restored controllers on retry. The new prepare_publish_striped entry acquires the queue, transfers the next map out of the transition State, composes collected preparation/reservation, restores the map and invokes the arbitrary-stripe publication driver. Busy return retains a handle-borrowing handoff and partial collection. Initial and retry attempts share resume_publish_striped; cancellation restores all leases and reconstructs State with the old map unchanged. A PendingHandoff now transfers the sealed pending map into actual collection while retaining the live map. Pending recovery polls those counters, takes the prepared queue and recovers every exact allocation under borrowed leases. Shared finish_rotation clears pending only after that callback returns, with complete collection and matching queue readiness; cancellation still retains pending. Native mapping, Cache and destruction remain incomplete. An admitted_read adapter now obtains its retained scope from an actual 32/64-bit gate CAS, loads the AtomicPtr, supports the heap-backed borrow and returns the observation before releasing the gate. Gate selection/domain membership remain caller obligations. It now retains one owned admission share per observation, validates reader-index tickets, and derives zero from matching sealed all-stripe histories before returning the exact heap permission. Persistent retirement bounds also connect shared generation publication to pending-only recovery while a current reader remains live. An arbitrary-stripe path covers every excluded gate with sealed idle histories and checks every pending stripe against the bound, using exact finite identity sets. The enriched AtomicPtr retirement record now passes through shared queue registration, per-record coverage preparation, certified detachment and batch heap recovery with its original allocation and completion owner preserved. A payload-preserving library queue lock now composes acquisition, preparation of the same records, publication through a borrowed matching barrier and release; its invariant restores allocation/owner/gate facts at acquisition. Shared certified detachment also uses actual queue handles, including holding both terminal queues through extraction; full-domain drain histories then recover their exact AtomicPtr allocations. Withdrawal receipts capture the protected source sequence and generation, and locked batch wrappers preserve that sequence through conversion to recovery work. Linear ready/prepared queue authority now retains the exact coverage bound across reacquisition and derives prepared payload facts for pending-only recovery; successful extraction consumes the prepared ticket and returns ready, while rejection preserves the ticket. Production Handle and Cache append/take expressions now share source with the queue backends; native SmallVec representation remains a separate obligation. Registration now runs the shared select/lock/recheck/retry/append expression against an actual vstd AtomicBool and queue WriteHandles. The atomic invariant owns the current ready token or the publishing side's phase-state token; uniqueness with the acquired queue state derives readiness at a matching recheck. Verified reserve/publish methods transfer these resources at the atomic operations, and Handle registers its exact enriched retirement entry through this backend. Handle's 32/64-bit adapter now requires the held transition and mapped atomic counters, composes queue acquisition, current recheck and record preparation, then invokes atomic_rotation's shared seal/pending/publish/reopen/barrier-release order. Stale selection preserves next-ready authority and the transition state. The former queue-only publication entry has been removed; the internal current store is called by the verified rotation drivers. A new arbitrary-stripe driver owns exact generation controller maps under its transition lock and composes actual all-stripe seal, pending, publication, reopen and queue release. Its reservation bound must equal the old generation gate set. Handle now has a collection/restoration handoff into this driver that consumes State while the next map is in collection custody; the original scalar/history entry remains alongside it pending migration. The vstd idle-publication path now passes its exact prepared queue records and retained DrainSet into Handle batch heap-permission recovery. Native AtomicU8 ordering, all-stripe identity, actual callback and destruction remain open. Native array/address/publication-driver and queue-lock correspondence, and destruction of the recovered batch, remain open. Native publication/removal additionally requires an exclusive borrow of the matching table write guard; native compile-fail checks reject early guard release and simultaneous writer capabilities. The mapping to verified ghost authority, Acquire/Release refinement and native striped-generation and queue instantiation of the retained-share registry remain open. Completion/debt/counter/batch/ownership/retention/read mutation checks cover these shared paths.
   - `PublishedOwner`, `OperationGate`, `ServiceSlot`: Protocol models and ownership tokens verified in Verus with 0 errors and 0 assumes. The shared admission-to-reclaim path now includes striped DrainGate collection, RotatingReadDomain publication and Cache/Handle recovery under modeled resources. Native primitive and field identity connections remain future work under the no-new-trusted-adapter policy.

---

The `just verus` gate also runs `tools/check_drain_gate_refinement.py`: five unsafe mutations of the shared control flow must fail proof obligations (release before lock, omitted notification, notification after unlock, return with active readers, and omitted re-registration). This checks proof sensitivity, not completeness of the backend refinement.

Initial DrainGate validation snapshot (2026-09-21, macOS aarch64; subsequent changes and evidence are recorded in [the worklist](REFINEMENT_WORKLIST.md)):

- `just verus`: all eight crates pass; the final DrainGate proof reports 81 verified obligations, including imported counter code and both word widths.
- `just verus-audit`: passes with zero `assume` and zero unapproved external bodies; all five negative refinement checks pass.
- `cargo test -p xlfn-kernel --lib --locked`: 66 tests pass, including the production-reusing Loom models.
- Kernel `miri_` tests on `nightly-2026-08-22`: 23 pass with default Stacked Borrows, and 23 with `-Zmiri-tree-borrows`. Both runs report a `parking_lot_core` integer-to-pointer provenance warning; these runs are not strict-provenance certification.
- Kernel Clippy with `-D warnings`, workspace formatting, and `git diff --check` pass. Windows execution and remote CI were not run for this change.

## 5. TCB & Quality Rules

1. **Zero Assumes**: Project-local `assume(...)`, `assume_specification`, and `axiom fn` declarations are strictly rejected in CI (`just verus-audit`).
2. **Controlled External Bodies**: External bodies and external type/function specifications must be registered in `tools/verus_external_allowlist.json`.
3. **Shared Kernels Where Present**: Directly verified transition and control expressions are shared with production. Independent protocol models and their remaining native refinement obligations are identified explicitly in the worklist.
4. **Zero Fast-Path Overhead**: All ghost specifications and proofs are completely erased at compilation time.

---

## 5. Usage

```bash
# Run Verus formal verification
just verus

# Audit verification codebase for assume / unapproved external_body violations
just verus-audit
```
