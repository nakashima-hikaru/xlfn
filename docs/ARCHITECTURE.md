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
permits and leases govern node reclamation. Moka and alternate sharded indexes
are available only through the internal benchmark feature.
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
