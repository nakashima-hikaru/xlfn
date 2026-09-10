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

| Owner | Owned Resources | Consumers and Lifetime |
| ------------------------ | ---------------------------------------------------------------------------------- | ---------------------------------------------------------------------------- |
| Module | Export ingress, DLL residency, host connections | Governed by admission permits and termination proofs |
| `Runtime<A>` | Lifecycle, host registration ledger, return tracking, executors, quarantined resources | Borrowed as required capabilities by call boundaries |
| Lifecycle generation bundle | `ExecutionGeneration` and `GenerationServices` | Retains published regions until `CallGuard` / `ExecutionLease` drain |
| `ThreadAffineSlot` | `A::LifecycleState` | Mutable access restricted to initialization and shutdown threads |
| `ExecutionGeneration` | `A::SharedState` and UDF layers | Shared-borrowed by UDFs within the lifetime of their guards |
| Generation services | Handle registry, RTD source / subscription | Referenced via generation-tagged IDs and service read permits |
| Handle registry | Binding table, read domain, object arena | Reclaimed independently after call-scoped borrows and async pins drain |
| RTD channel adapter | Source owns factory; subscription workers own individual `FnOnce` jobs | Subscription closes the queue and joins producer / publisher workers |
| Return protocol | Excel return values allocated by the DLL | Reclaimed by `xlAutoFree12`; physical unload is prohibited while uncollected |

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
