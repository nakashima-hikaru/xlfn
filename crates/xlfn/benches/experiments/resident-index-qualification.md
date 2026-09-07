# Production resident index: Quick Cache

On 2026-09-07, the production decision was changed to Quick Cache 0.7.0 with
one native shard and xlfn's 104-line safe shared-flight core. The measured
[phase-two tradeoffs](quick-cache-shared-flight.md) were explicitly accepted:
the six-read geometric mean improved 76%, mixed32 throughput 34%, eviction32
19%, and invalidate32 168%. Mixed32 hit p99 rose from about 6.9 to 26 µs, writer
p99 rose 3.4%, invalidate32 peak debt rose from 65 to 81 nodes, and churn32
throughput fell 12–16%. These are local Apple M1 results with 32 threads on
eight logical CPUs, not a universal performance guarantee.

The earlier no-regression selection gates rejected the candidate, but are not
production requirements. No further backend comparison or hardware campaign
was required for this decision. Implementation is now solely:

```text
CalculationCache + shared flights
    -> Quick Cache resident index (one shard)
```

Resident entries remain `(NodePtr<V>, u32)`. Creator/lease pins, generation,
read-domain admission, residency retirement, queues, reclamation and
backpressure retain their existing protocol. Native lifecycle request states
invoke the existing retirement callback after unlocking. There is no custom
resident capacity or eviction policy. Moka's deferred maintenance machinery
and backend selection are gone.

Flights are distinct from residency, contain no node pointers, and are removed
on success, error and panic. Successful waiters relookup through the existing
admission domain. Errors are shared within the same flight; panic permits retry.
The registry retains reusable table capacity after removing completed flights.

Permanent regressions enforce exact resident capacity, same-flight error
sharing, panic/retry, exactly-once payload destruction and final zero retirement
debt. Existing mutation backpressure still waits when pending nodes reach 256
or pending weight reaches the cache budget; concurrent operations and retained
leases mean this is not a strict process-memory limit. `just miri-cache`, also
in CI, runs the full-cache protocol and safe-flight tests under both borrow
models without disabling leak or alias checks. See [Miri scope](cache-miri.md).

Production benchmarks use fresh `cache_lookup/quick` and
`cache_reclamation/quick` names in `just bench-cache` and the normal CI benchmark
history. Future regressions compare against Quick's history, with no executable
Moka-relative gates. Moka-only diagnostic controls, all backend selectors and
comparison harnesses/dependencies are removed; historical evidence stays in Git.

The normal `unstable-cache` dependency graph no longer includes Moka or
`crossbeam-epoch`. The separate `async -> crossbeam-deque -> crossbeam-epoch`
path remains. This is a cache-path removal, not a workspace-wide epoch removal.

## Adoption verification

The final production implementation passed 793 workspace tests (7 existing
ignored tests), formatting, workspace/all-target Clippy, all 36 depth-two
feature combinations, panic-boundary inventory, dependency policy checks and
guide validation. Sixteen cache/flight regressions passed each Miri borrow
model, including the actual 256-node backpressure threshold; the parking_lot
provenance warning remains recorded. Windows i686/x86_64 MSVC library Clippy
passed with `blake3/pure`; no additional Windows/Excel hardware campaign ran.
All 36 lookup and 12 reclamation benchmark cases passed untimed smoke checks.
The first normal CI benchmark run will seed the new Quick-only history.
