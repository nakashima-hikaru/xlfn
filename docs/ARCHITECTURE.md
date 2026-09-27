# Architecture and Ownership

## Components

`xlfn` is the execution runtime for Excel XLLs. `xlfn-macros` generates
registration metadata and ABI boundaries from typed UDFs, `xlfn-sys` provides the
Excel / Windows ABI, `xlfn-common` defines shared declaration conventions, and
`xlfn-kernel` provides synchronization primitives for borrowing, publication, and
draining. `xlfn-package` handles verified bundle creation and staging, while
`cargo-xlfn` handles Cargo execution and packaging operations. `formal/`
cross-checks abstract models of shutdown, generations, and handles against Rust
execution traces. Live Windows / Excel validation forms a separate verification
boundary.

## Ownership Structure

| Owner                       | Owned Resources                                                                        | Consumers and Lifetime                                                       |
| --------------------------- | -------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------- |
| Module                      | Export ingress, DLL residency, host connections                                        | Governed by admission permits and termination proofs                         |
| `Runtime<A>`                | Lifecycle, host registration ledger, return tracking, executors, quarantined resources | Borrowed as required capabilities by call boundaries                         |
| Lifecycle generation bundle | `ExecutionGeneration` and `GenerationServices`                                         | Retains published regions until `CallGuard` / `ExecutionLease` drain         |
| `ThreadAffineSlot`          | `A::LifecycleState`                                                                    | Mutable access restricted to initialization and shutdown threads             |
| `ExecutionGeneration`       | `A::SharedState` and UDF layers                                                        | Shared-borrowed by UDFs within the lifetime of their guards                  |
| Generation services         | Handle registry, RTD source / subscription                                             | Referenced via generation-tagged IDs and service read permits                |
| Handle registry             | Binding table, read domain, object arena                                               | Reclaimed independently after call-scoped borrows and async pins drain       |
| RTD channel adapter         | Source owns factory; subscription workers own individual `FnOnce` jobs                 | Subscription closes the queue and joins producer / publisher workers         |
| Return protocol             | Excel return values allocated by the DLL                                               | Reclaimed by `xlAutoFree12`; physical unload is prohibited while uncollected |

Excel inputs are borrowed only for the duration of a call. Asynchronous operations
receive owned inputs and an execution lease (`ExecutionLease`). Handle identifiers
are not ownership tokens themselves, but keys for obtaining scoped borrows verified
for generation, type, and validity.

## Opening and Teardown

During opening, `OpeningTxn` owns state, service inputs, and the registration journal,
publishing the full generation only upon success. On interruption, responsibility
transfers to rollback or quarantine.
Once `Addin::open` starts, a distinct initializing transaction cannot produce
an empty-addin rollback proof. If the hook returns an error without state,
the runtime is quarantined and retains DLL residency because application
execution sources cannot be quiesced. A successful hook transfers its state
to the ordinary rollback-capable transaction.
During teardown, a single removal claim sequences ingress closure, draining execution
and return producers, stopping async tasks and subscriptions, host unregistration and
callback ingress closure, add-in quiescence, service sealing, cleanup, and reclamation,
diagnostics draining, and RTD module draining, aggregating proofs to certify final
closure. Resources whose stoppage cannot be proven are retained in quarantine.

Standard shutdown is a logical termination. Physical unload requires the explicit
contract of `PhysicallyUnloadableAddin` alongside a termination certificate.
`PublishedOwner` provides a stable address with unique ownership, and pointer validity
lifetimes are strictly bounded by gates, leases, and joins.

## Resident and publication indexes

Calculation-cache residency uses Quick Cache with a single global weight budget.
The index owns one residency pin per stored entry; lookup snapshots are non-owning.
Eviction, rejection, explicit invalidation and clear release that pin and enqueue
retirement without running user destructors inside index locks. Existing read
permits and leases govern node reclamation. Alternate sharded indexes
are available only through the internal benchmark feature.
Stored keys have a separate retirement queue: their destructors run after index
and clear locks are released, with each panic contained independently. Nested
initialization and lookups defer this work until the outer caller leaves; value
backpressure also avoids waiting on the calling thread's own read admission.
Single-flight leaders retain registration ownership until removal completes.
Removal uses the registration's cached hash and allocation identity, so neither
normal completion nor unwind cleanup calls user key hashing or equality again.
Zero-budget nodes are never published and belong to their single lease. They
can be destroyed directly, except during cache initialization, when destruction
is deferred until the initialization guard has exited.

Handle-topic publication uses Papaya for short map lookups. Its guard protects
map slots while copying a pointer; the runtime's rotating read permit protects
the separately owned topic. Transactional topic changes remain under the topic
table write lock. Withdrawal and retirement registration precede publication of
the next read generation through the retirement-queue publication barrier.

Canonical RTD topic parts use immutable `SmolStr` values internally. The public
`RtdTopic` API accepts `AsRef<str>` inputs and exposes borrowed `&str` parts
through `part` and the exact-size `RtdTopicParts` iterator. Borrowed lookup stays
allocation-free on a hit. Pending updates use dense `IndexMap` entries so refresh
traversal follows current updates rather than retained hash capacity. The serial
refresh transaction recycles its empty output buffer after completion or rollback.
Server termination releases table and refresh-buffer allocations while retaining
the small server identity needed to reject stale handles. Observation encodes its
fixed-width transport key into an inline ASCII buffer.

Handle binding publication allocates stable pages of 256 slots as they are used.
The page directory depends on the configured cap; slot allocation and withdrawal
depend on actual usage. Departing readers with no queued retirement avoid shared
maintenance writes. An acquire fence preserves the seal/release handoff, and a
counted notification protocol gives one borrowing caller responsibility for draining
all maintenance requests. Small binding/topic retirement batches remain inline.

`xlAsyncReturn` borrows an inline root only for its synchronous callback. Its string
and array storage remains owned by the callback value; it never enters the
Excel-owned `ReturnBlock` / `xlAutoFree12` protocol. Async task shards own their
control maps and are padded independently; only cancellation computes the capacity
needed to drain them.

`PeSnapshot` owns immutable bytes together with their parsed PE information. CRT
inspection borrows that information, then package verification consumes the same
snapshot and compares it against the staged XLL without allocating another image.
Package artifacts bind immutable shared bytes, size and digest at construction.
Subsequent commit checks compare every file byte against that snapshot, preserving
identity and stable-read checks without recalculating the same digest. File
diagnostics apply the text budget during formatting before JSON serialization.
Diagnostic queue capacity is reserved before cloning the event; failed admission
does not build a payload. Construction unwind returns its reservation, successful
send transfers it to the worker, and dequeue returns it before sink delivery.

## Unsafe implementation review

These rules apply when changing the framework's raw-pointer publication and
reclamation implementation. Public application contracts remain in the guide.

Cache, handle, RTD, and async publication use unique owners and counted read
capabilities. The temporal ownership models describe the required lifetime
protocol; their proofs do not establish Rust pointer provenance or atomic
memory ordering. Those obligations also require implementation review, Loom
models, and Miri tests.

Any code that retains a raw pointer or provides lease-based access across concurrent operations must explicitly document and implement the following five criteria:

1. **Owner**: Identifies the single unique owner holding primary ownership of the allocation.
2. **Admission Mechanism**: Defines the short-lived admission gate (e.g. `StripedDrainGate` domain permit) entered *before* observing the raw pointer, preventing use-after-reclaim during concurrent retirement.
3. **Retirement Point**: The explicit transition where the object is marked retired (e.g. cache eviction, binding removal, callback replacement). No new lookups can acquire pins after retirement.
4. **Reclamation Point**: The deferred deallocation point executed only after retirement, complete quiescence of the lookup admission domain (`admissions == 0`), and release of all active pins (`pins == 0`).
5. **Formal Invariant ID**: An explicit reference tag in code (such as `[TR-OBSERVE-POINTER]`, `[TR-LEASE-1]`, `[TR-RECLAIM-1]`) linking the unsafe block to its corresponding Lean 4 theorem and Loom exhaustive model.

Published allocations whose owners can move use the kernel's
`PublishedOwner<T>`. It retains unique allocation ownership as a raw pointer,
exposes shared access, and recovers a `Box` only after the relevant readers
have drained. A stable heap address alone is insufficient: moving a `Box`
while raw readers use its allocation can invalidate their aliasing permissions.

Drain gates register their notification requirement in the same atomic state
as the active count. A notifying final release holds the wait mutex before
publishing zero, and a drain waiter synchronizes with that release's final
access. Async generation snapshots acquire a lifetime pin before dereferencing
the current generation; completion retains the pin even after cancellation
removes the task's control entry. Executor destruction cancels, drains, and
joins workers before reclaiming the uniquely owned executor allocation.
