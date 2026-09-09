# Protocol cost experiments — 2026-09-09

Implemented: `CacheNode<V>` now stores `V` inline. The isolated `u64` entry probe measures **2 → 1 allocations** and **48 → 40 requested bytes**. Destruction drops the owning node in place inside `catch_no_unwind`, after framework locks and grace-period callbacks have been released. This avoids moving a large payload into the unwind closure. The resident storage estimate excludes the inline payload before adding the caller-reported weight, preventing double counting.

This file records the initial prototype experiment. See [production follow-up](protocol-production-2026-09-09.md) for the later single-state-check, deferred-retirement, and RTD edge+batch adoption and validation. Prototype patches below are historical snapshots, not patches against the current production tree.

At the end of this initial experiment, all other candidates remained unapplied patches. Repeated measurements do **not** justify promoting combined resident/pin state: its approximately 1% single-thread gain did not reproduce as a hot32 gain. Deferred handle retirement substantially lowers removal latency with a retained call scope, while transferring the wait to final drain. The RTD queue candidates improve the tested bounded-queue workload.

## Scope and measurement

Base: `a65833b112edf1d01fd3c2703d858ab87b56c6b7`. Apple M1, macOS 26.6.2, Rust 1.98.0, aarch64-apple-darwin, System allocator. Existing uncommitted subscription/RTD changes were preserved. Raw estimates, confidence intervals, allocation counts, latency samples summarized as quantiles, retirement debt, and patch hashes are in [the results JSON](protocol-costs-2026-09-09-results.json).

Confirmation executables were built first, then run serially without concurrent builds or tests. This is a short local experiment, with no CPU affinity or Windows qualification. Lookup uses 1 s measurement, 100 ms warm-up, 30 samples, and three alternating A/B runs. Reclamation uses 500 ms, 100 ms warm-up, 20 flat samples, and two runs in opposite variant order. These observations are not a formal noninferiority gate.

## Inline cache storage

Cache reclamation uses 256 insertions per worker and a 16-entry weight budget. `Payload(Box<[u8]>)` contains a **heap-backed** 64 B or 64 KiB value. Its backing allocation remains. The benchmark does not establish allocator footprint or performance for large inline arrays; the size-class/alignment regressions in [the historical layout experiment](cache-node-layout.md) remain relevant. `Box<ZST>` never required a separate allocator call, so the 2 → 1 statement applies to nonzero-sized payloads.

Times below are the mean of two Criterion batch medians. p99 columns show the range of the two independent latency-probe p99 values; they are not pooled quantiles. Allocation/probe work is outside Criterion timing.

| Workload | Payload | Threads | Boxed batch ms | Inline batch ms | Boxed p99 µs | Inline p99 µs |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| churn | 64 B | 1 | 0.377 | 0.375 | 16.83–16.96 | 14.42–16.08 |
| churn | 64 B | 8 | 2.918 | 2.788 | 222.83–253.71 | 188.12–233.21 |
| churn | 64 B | 32 | 11.783 | 11.346 | 1101.96–1320.50 | 1145.29–1198.33 |
| churn | 65,536 B | 1 | 0.651 | 0.659 | 17.71–19.62 | 18.67–19.83 |
| churn | 65,536 B | 8 | 4.187 | 4.096 | 218.17–243.17 | 245.79–260.29 |
| churn | 65,536 B | 32 | 16.847 | 16.537 | 1566.21–1621.38 | 1569.50–1634.04 |
| live_leases | 64 B | 1 | 0.455 | 0.454 | 14.29–14.58 | 14.29–14.79 |
| live_leases | 64 B | 8 | 5.967 | 5.887 | 166.33–173.33 | 157.29–170.12 |
| live_leases | 64 B | 32 | 29.949 | 29.670 | 725.83–736.83 | 672.00–674.88 |
| live_leases | 65,536 B | 1 | 0.730 | 0.733 | 13.92–14.92 | 13.21–15.67 |
| live_leases | 65,536 B | 8 | 6.518 | 6.461 | 159.88–177.83 | 145.25–157.67 |
| live_leases | 65,536 B | 32 | 32.520 | 32.389 | 761.08–787.00 | 541.75–729.58 |

Allocation reduction is reproducible; throughput and tail changes are workload dependent. For example, 64 B churn at 32 threads drops from roughly 147.8k to 139.8k allocator calls per 8,192-operation probe. Moka policy/background work makes these whole-cache counts noisy; the zero-capacity probe isolates exactly one saved entry allocation. Pending-debt peaks remain schedule sensitive (up to 385 boxed and 383 inline nodes in these probes). No tighter memory-debt bound is claimed.

## Combined resident/pin atomic

[Patch](cache-combined-state.patch), relative to the inline implementation. The high bit represents residency and the remaining bits include the residency pin. Successful pin acquisition requires residency and an unsaturated count. Unique residency withdrawal subtracts the residency bit and one pin in one RMW; final lease release claims reclamation only when its prior state was one. The read domain and generation ordering remain unchanged. A pin that linearizes before withdrawal may succeed.

Times are median-of-three batch medians, with the observed run range in parentheses. Each batch has 1,000 hits per worker.

| Case | Split state µs | Combined state µs | Change |
| --- | ---: | ---: | ---: |
| cache_hit_hot_key_current/threads_1_u64 | 115.19 (114.80–115.70) | 113.93 (113.49–114.13) | -1.10% |
| cache_hit_disjoint_current/threads_1_u64 | 115.44 (114.93–115.71) | 114.20 (113.89–114.82) | -1.07% |
| cache_hit_hot_key_current/threads_32_u64 | 6105.55 (5686.73–6278.00) | 6252.65 (5185.79–6894.32) | +2.41% |
| cache_hit_disjoint_current/threads_32_u64 | 3970.12 (3955.89–4185.60) | 4050.40 (2844.23–4072.03) | +2.02% |

The initial hot32 improvement disappeared in the alternating repeats. This candidate is rejected and archived; no further combined-state work is planned. Its initial reclamation probes, including p99 and debt, are in `combined_reclamation_initial` in the JSON; those were collected during other validation work and are diagnostic only, not an acceptance comparison. The new Loom model checks final-owner uniqueness for the combined state machine, not the entire Moka implementation.

## Handle lookup and retirement

[Single-state-check control](handle-single-state-check.patch) removes only the two post-`read_scoped` Live checks. Type checking and call-scope lifetime protection remain. The successful Live observation in `read_scoped` is the linearization point. Initial batch medians were:

| Threads | Three checks µs | One check µs |
| ---: | ---: | ---: |
| 1 | 24.10 | 23.63 |
| 32 | 210.34 | 203.38 |

This is one initial comparison; the gain is small enough to require repeated target-host validation before promotion. The benchmark now asserts successful lookup rather than silently timing an error.

**Safety follow-up:** the archived deferred prototype lacks the registration/publication barrier established in the [production follow-up](protocol-production-2026-09-09.md). A later Loom model demonstrates why its generation recheck alone is insufficient on weak memory. Do not use this historical patch as a production implementation; the original Miri runs did not establish that ordering property.

[Deferred-retirement patch](handle-deferred-retirement.patch) preserves the existing lookup path, including all three state loads, and adds generation-bound queues to the existing domain. Enqueue occurs before releasing the binding-table writer lock so `retire_all`/seal cannot miss a removal whose publication has already been withdrawn. Every 32 removals, maintenance attempts `try_quiesce_if_idle`. Queue/transition locks are released before arbitrary destructors run. Seal drains both generations.

Each removal probe has 100 operations, with initialization/thread setup outside its timer. The retained-reader case establishes an actual borrowed handle before timing removal, then sleeps for 1 ms in that reader. Timers include scheduling overhead; requested sleep is not an exact 1 ms grace period. Final drain includes registry seal and object-quiescence validation.

| Reader hold | Policy | Remove p50 µs | Remove p99 µs | Final drain p50 µs | Final drain p99 µs |
| --- | --- | ---: | ---: | ---: | ---: |
| 0 µs | current | 0.833 | 1.750 | 0.625 | 0.625 |
| 0 µs | deferred | 0.500 | 1.292 | 0.708 | 0.792 |
| 1000 µs | current | 1264.667 | 1279.875 | 1.041 | 1.542 |
| 1000 µs | deferred | 1.458 | 2.375 | 1264.125 | 1275.541 |

All 100 payloads were destroyed exactly once after final drain for each case. Deferred removal retained all 100 until subsequent drain; immediate removal retained none. Three alternating lookup comparisons of the final deferred patch show:

| Threads | Current µs/batch | Deferred µs/batch | Change |
| ---: | ---: | ---: | ---: |
| 1 | 23.67 | 23.63 | -0.15% |
| 32 | 207.18 | 207.64 | +0.22% |

This is consistent with unchanged lookup work, but does not prove strict noninferiority. The prototype has no background maintenance or retirement-debt cap: fewer than 32 removals may retain values until seal, and persistent readers can indefinitely postpone idle maintenance. It does not implement split-phase rotation while readers are active. Production promotion needs a maintenance trigger/backpressure policy and revised tests for the deliberately changed eager-destruction semantics.

## RTD channel queue

[Edge patch](rtd-channel-edge.patch) adds an atomic early admission check, retains the authoritative check under the queue lock, and sends `notify_one` only on empty → nonempty. Cancellation waiters get a separate Condvar; using the existing shared Condvar with `notify_one` could wake `wait_closed` instead of the publisher and strand data. Close and producer completion broadcast to both wait classes.

[Batch patch](rtd-channel-batch.patch), applied **after** the edge patch, dequeues up to 32 stored values per lock acquisition. An atomic stop flag lets disconnect discard locally dequeued values except for an already in-flight publication.

The probe uses the actual conversion/queue/send/receive code, with 10,000 values per producer, one consumer, bounded capacity, and `yield_now` retries on overload. Threads are created before the timing barrier; final worker joins are included. Each process discards its first round and records six round durations. Below is the mean of two process medians, run in opposite variant order. This is queue throughput, not Excel/COM publication throughput or a many-topic topology test.

| Producers | Capacity | Current ms | Edge ms | Edge + batch ms |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 1 | 26.261 | 25.942 | 26.279 |
| 1 | 64 | 0.702 | 0.584 | 0.473 |
| 1 | 1024 | 0.582 | 0.456 | 0.379 |
| 4 | 1 | 182.422 | 172.328 | 117.103 |
| 4 | 64 | 5.567 | 5.035 | 3.813 |
| 4 | 1024 | 3.076 | 2.443 | 1.896 |

Capacity-one results are dominated by wake/scheduling/retry behavior; batching cannot dequeue more than capacity there. The two OS threads per production subscription are unchanged. `kanal` and shared-publisher topology are subsequent experiments and were not implemented or measured in this change. Neither queue result is evidence for their performance. Token-cache associativity and generation-order relaxation are also unchanged.

## Validation

- Final formatting, all-target/all-feature xlfn Clippy with warnings denied, and the panic-boundary audit pass (44 reviewed direct references; all five audit tests pass).
- Final implementation: all-feature xlfn tests, 582 passed / 7 ignored; UI suite passed. Cache-focused native tests: 67 passed, including aligned 8 KiB inline storage, stable addresses across clear, exactly-once drop, reentrant drop, and panic containment.
- Inline cache: all 11 sharded-backend full-cache tests pass in Miri Stacked Borrows and Tree Borrows. The Moka dependency path remains [separately unqualified](cache-miri.md); these results must not be described as full-Moka Miri qualification.
- Combined candidate: 69 cache tests pass, including overflow/retired-pin tests and the added Loom state-machine model.
- Single-check candidate: 97 handle tests pass, 1 ignored.
- Deferred candidate: three focused tests pass natively and under Miri, covering retained borrows with 65 removals/slot reuse, debt below the threshold at seal, and seal racing removal. The existing eager-removal tests have not been rewritten for this prototype.
- RTD edge and batch candidates: 11 channel tests pass each, including cancellation waiters sharing the channel with a waiting publisher, close during conversion, finite producer drain, disconnect joins, and panic containment.

## Reproduce

Historical reproduction requires the initial inline baseline from this experiment, before the production follow-up. The saved patches must not be applied to the current production sources. Apply each independently to that historical inline baseline; the RTD batch patch is the sole exception and depends on the edge patch.

```sh
XLFN_BENCH_MEASUREMENT_MS=1000 cargo bench -p xlfn --bench cache_lookup \
  --features bench-internals,unstable-cache -- \
  "cache_hit_(hot_key|disjoint)/current/threads_(1|32)/" \
  --noplot --warm-up-time 0.1 --sample-size 30
XLFN_BENCH_MEASUREMENT_MS=500 cargo bench -p xlfn --bench cache_reclamation \
  --features bench-internals,unstable-cache -- --noplot --warm-up-time 0.1
XLFN_BENCH_MEASUREMENT_MS=1000 cargo bench -p xlfn --bench handle_lookup \
  --features bench-internals -- "warm_same_token/(1|32)$" \
  --noplot --warm-up-time 0.1 --sample-size 30
cargo bench -p xlfn --bench protocol_costs --features bench-internals,rtd
```

For example, `git apply crates/xlfn/benches/experiments/cache-combined-state.patch` enables the combined candidate, and `git apply -R` with the same path removes it. For the boxed comparison, use `cache.rs` from the recorded base revision with the current reclamation harness. Preserve the same compiler, features, payloads, and worker topology for both variants. Normal benchmark runs retain the default ten-second measurement policy when the environment override is omitted.
