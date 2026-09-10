# Benchmark notes

## `cache_lookup`

This benchmark compares the production `CalculationCache<u64, u64>` with a
benchmark-only control that keeps the same versioned Moka cache, capacity, key
shape, warm hit rate, and persistent worker topology, but stores `(Arc<u64>,
weight)` instead of the production node pointer and lease protocol.

The lookup cases are:

- one warm key with one worker;
- one hot key with 1, 2, 8, and 32 workers;
- one disjoint key per worker with 1, 2, 8, and 32 workers;
- deterministic live-lease retirement with a one-entry cache.

The steady-state hit rows also include two benchmark-only diagnostic controls:

- `no_admission_control`: the same Moka lookup and node pin, without lookup admission;
- `no_pin_control`: the same Moka lookup and lookup admission, with raw node access and no pin accounting.

Together with `current` and `arc_control`, these controls separate admission and
pin costs without adding either mechanism to the production cache API. The
diagnostic controls assume that the warmed cache is not evicted or mutated
while the worker pool is running.

Run the full benchmark with the normal ten-second measurement policy:

```text
cargo bench -p xlfn --bench cache_lookup --features bench-internals,unstable-cache
```

For a short local smoke run, set `XLFN_BENCH_MEASUREMENT_MS` and reduce the
warm-up time:

```text
XLFN_BENCH_MEASUREMENT_MS=50 cargo bench -p xlfn --bench cache_lookup \
  --features bench-internals,unstable-cache -- --noplot --warm-up-time 0.05
```

The allocation probe runs outside Criterion's timed section and reports
allocator calls from one warm batch after the worker pool and cache have
already been initialized.

## 2026-09-05 smoke result

The lookup rows below used 50 ms measurement time, 100 samples, and 50 ms
warm-up. The live-lease row used the same measurement time with Criterion's
default warm-up. Values are the time for one benchmark batch; each batch
contains 1,000 hits per worker. These numbers validate the benchmark and are
not a replacement for a full ten-second comparison on the target deployment
host.

| Case | Current | Arc control |
| --- | ---: | ---: |
| warm, 1 worker | 114.97 µs | 107.50 µs |
| hot key, 1 worker | 114.87 µs | 109.08 µs |
| hot key, 2 workers | 276.75 µs | 279.50 µs |
| hot key, 8 workers | 1.1808 ms | 1.6017 ms |
| hot key, 32 workers | 4.7483 ms | 8.5208 ms |
| disjoint, 1 worker | 115.57 µs | 109.10 µs |
| disjoint, 2 workers | 228.12 µs | 209.56 µs |
| disjoint, 8 workers | 960.78 µs | 955.29 µs |
| disjoint, 32 workers | 3.8993 ms | 3.8682 ms |
| live-lease retirement, 1 worker | 699.19 µs | 588.79 µs |

The warm-hit allocation probe reported 8 allocations for current and 0 for
the Arc control in this run. The current-path allocation count is diagnostic
and should be investigated separately from the ownership comparison; it is
not folded into the Criterion timing conclusion.

## 2026-09-05 current vs Arc full run

The requested ten-second run completed before adding the diagnostic controls.
Values are the median time for one batch of 1,000 hits per worker:

| Case | Current | Arc control |
| --- | ---: | ---: |
| warm, 1 worker | 114.89 µs | 108.35 µs |
| hot key, 1 worker | 114.38 µs | 108.42 µs |
| hot key, 2 workers | 263.88 µs | 262.21 µs |
| hot key, 8 workers | 1.4032 ms | 1.6133 ms |
| hot key, 32 workers | 5.9926 ms | 7.0730 ms |
| disjoint, 1 worker | 116.89 µs | 109.68 µs |
| disjoint, 2 workers | 216.68 µs | 220.02 µs |
| disjoint, 8 workers | 1.1044 ms | 1.1793 ms |
| disjoint, 32 workers | 4.9006 ms | 4.1825 ms |
| live-lease retirement, 1 worker | 691.61 µs | 576.87 µs |

## 2026-09-05 diagnostic run

The diagnostic rows used 1 s measurement time and 50 samples, with a 3 s
follow-up for the 32-worker rows. Values are batch medians:

| Case | Current | No admission | No pin | Arc control |
| --- | ---: | ---: | ---: | ---: |
| warm, 1 worker | 116.79 µs | 93.75 µs | 95.80 µs | 110.44 µs |
| hot key, 1 worker | 116.65 µs | 93.50 µs | 95.96 µs | 109.71 µs |
| hot key, 32 workers | 6.6656 ms | 6.5591 ms | 6.1722 ms | 7.3071 ms |
| disjoint, 1 worker | 116.97 µs | 94.42 µs | 96.15 µs | 110.16 µs |
| disjoint, 32 workers | 4.2866 ms | 4.0903 ms | 3.9854 ms | 4.0644 ms |

There is no `miss_singleflight` row in this benchmark yet.

## Scoped-read diagnostic

The benchmark also exercises the phase-1 `CacheReadScope` path. It is gated by
`bench-internals` and is not part of the production cache API yet. The scope
holds one lookup-domain permit, uses borrowed `VersionedKeyRef` lookups, and
avoids per-node pin increments and decrements while the lexical scope is alive.

- `scoped_per_lookup`: one scope for each hit;
- `scoped_batch`: one scope for the whole 1,000-hit worker batch;
- `scoped_duration/lookups_N`: one scope with `N` repeated hits, to expose the
  cost of keeping an observation scope open;
- `concurrent_clear_latency/scope_N`: clear reaches reclamation while the
  scoped reader is held, then the reader is released through a benchmark-only
  synchronization hook. Its time includes coordination overhead and is a
  protocol diagnostic, not a production clear-latency SLA.

One local 1 s / 50-sample warm-hit run measured 114.34 µs for `current`,
110.99 µs for `scoped_per_lookup`, 108.03 µs for `scoped_batch`, and
106.86 µs for `arc_control`. This suggests that batching the observation
permit removes a small recurring cost, while a scope per lookup is only about
3% faster than the current lease path in this setup. The 32-worker scoped rows
were scheduler-sensitive and should be re-measured on the target host before
they are used for a design decision.

A separate 500 ms / 30-sample run measured `scoped_duration/lookups_N` at
approximately 2.56 µs, 3.48 µs, 12.59 µs, and 97.98 µs for
`N = 1, 10, 100, 1,000`, respectively. These are batch times for one worker;
Criterion throughput normalizes them by the number of hits. A separate 250 ms
/ 15-sample clear-latency run measured approximately 17.3 µs, 18.0 µs, 25.5
µs, and 87.6 µs for the same `N` values, showing the expected cost of holding
the active scope while it performs post-clear lookups.

## `cache_reclamation`

This benchmark measures eviction/reclamation through the ordinary
`CalculationCache::get_or_try_insert_with` and `CacheLease::drop` APIs. It does
not call `len`, `used_weight`, `clear`, or a benchmark-only read scope in any
measured workload. Every operation uses a fresh, worker-disjoint key.

The matrix contains 1, 8, and 32 persistent workers, each with 64-byte or
64-KiB payloads, and two separate workloads:

- `churn`: release each returned lease within the insertion operation;
- `live_leases`: retain a ring of 32 leases per worker, releasing the displaced
  lease within each insertion operation. The ring survives between batches.

The resident weight budget is 16 payloads in every case. Retained leases can
keep evicted values alive intentionally; `held_lease_payload_bytes` reports
that population separately from pending reclamation. It can overlap resident
values and must not be added to resident weight as if the two were disjoint.

Each case emits one machine-readable `cache_reclamation_probe` JSON line
before Criterion's measurements. The probes use separate batches:

- `allocation_probe`: count allocator/reallocator requests and requested bytes
  on the worker threads while they perform cache operations. Thread creation,
  retention buffers, diagnostic buffers, and driver allocations are excluded.
  Requested bytes are cumulative allocation traffic, including the new size
  of reallocations, and are not live memory or process RSS.
- `latency_probe`: report nearest-rank p50/p99/max over individual operations,
  including allocation, insertion, lease displacement/drop, and any synchronous
  reclamation they trigger. Allocation counting is disabled for this batch.
  `reclamation_stats()` is sampled after each operation's timer stops; it only
  reads counters and cannot advance Moka maintenance or a grace period.
- `cache_lifetime_stats_after_probes`: report the cache's intrinsic peak
  pending counts/weights, final pending values, reclaimed nodes, largest batch,
  and cumulative grace-period time. These include the one warm-up batch and
  both diagnostic batches. Sampled per-operation peaks may miss shorter spikes;
  the cache's intrinsic peaks capture enqueue events between observations.

Criterion separately times ordinary batches, without per-operation timers,
allocation counting, or statistics sampling. Worker coordination is included
in batch time, while thread startup and shutdown are excluded. The counting
allocator's disabled TLS check remains present. Final retained leases are
released at worker shutdown, outside measurement; steady-state displaced-lease
release is included in both the operation and batch timings.

Run the normal ten-second-per-case measurements with 256 operations per worker
and 20 flat samples:

```text
cargo bench -p xlfn --bench cache_reclamation --features bench-internals,unstable-cache
```

For a short execution/metrics smoke check of all twelve cases, use 64
operations per worker and Criterion's test mode:

```text
XLFN_CACHE_RECLAIM_OPERATIONS=64 cargo bench -p xlfn --bench cache_reclamation \
  --features bench-internals,unstable-cache -- --test
```

`XLFN_CACHE_RECLAIM_OPERATIONS` accepts 64 through 65,536 operations per worker.
For a brief timed exploration, `XLFN_BENCH_MEASUREMENT_MS=50` and
`--warm-up-time 0.05 --noplot` reduce Criterion's measurement policy. Diagnostic
probes still run for every matrix case, even when a Criterion name filter is
supplied.

Smoke p99 values contain only 64 samples for one worker and 2,048 for 32
workers. They validate the instrumentation and expose obvious stalls; they do
not establish production tail-latency bounds or a performance improvement over
another implementation. The latency probe also perturbs scheduling through
clock/statistics reads. Compare repeated full runs on the deployment host,
with matching allocator, payloads, retention windows, and cache policy, before
using the results for a design decision.

### 2026-09-07 reclamation smoke result

A local macOS arm64 release build completed all twelve cases in test mode,
using 64 operations per worker. Each final pending-node count was zero after
the diagnostic batches, without an observer-triggered cleanup. The table
records one short run; allocator counts and p99 come from separate probes.
Peak pending nodes cover warm-up plus both probes, while retained leases are
reported at the end of the latency probe.

| Workload | Payload | Workers | Allocation requests / batch | Operation p99 | Peak pending nodes | Retained leases |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| churn | 64 B | 1 | 1,173 | 13.8 µs | 31 | 0 |
| churn | 64 B | 8 | 9,242 | 147.2 µs | 32 | 0 |
| churn | 64 B | 32 | 36,868 | 1,120.9 µs | 154 | 0 |
| churn | 64 KiB | 1 | 1,159 | 16.7 µs | 31 | 0 |
| churn | 64 KiB | 8 | 9,217 | 218.8 µs | 65 | 0 |
| churn | 64 KiB | 32 | 36,954 | 1,232.5 µs | 59 | 0 |
| live leases | 64 B | 1 | 1,197 | 10.8 µs | 16 | 32 |
| live leases | 64 B | 8 | 9,482 | 130.0 µs | 11 | 256 |
| live leases | 64 B | 32 | 37,770 | 698.1 µs | 35 | 1,024 |
| live leases | 64 KiB | 1 | 1,165 | 10.7 µs | 16 | 32 |
| live leases | 64 KiB | 8 | 9,463 | 134.4 µs | 8 | 256 |
| live leases | 64 KiB | 32 | 37,739 | 721.8 µs | 46 | 1,024 |

This run did not compare an earlier implementation, and Criterion test mode
does not produce throughput estimates or confidence intervals. The 32-worker
p99 includes substantial contention/scheduling effects; it is a measurement
starting point, not evidence that tail latency has improved.

## `rtd_prepare`

This benchmark times batches of 256 subscriptions on one source, with either
one or ten topic parts:

- `new`: prepare and commit distinct pending subscriptions;
- `existing`: prepare and commit topics already present as committed pending entries;
- `churn`: prepare the entire batch, then roll back every reservation.

Criterion batched setup constructs the runtime and input topics outside the
timed section. Existing-entry seeding and final runtime teardown are also
excluded. Timing includes reservation handling, index maintenance, pending-byte
accounting, and destruction of consumed topics; churn also includes its temporary
reservation vector. It does not include Excel/COM connection or publication.

Run all six cases with the normal ten-second measurement policy:

```text
just bench-one rtd_prepare "bench-internals rtd"
```

`just bench-full` includes this target. For a shorter local A/B comparison, use
the same benchmark fixture on both revisions and save separate Criterion baselines:

```text
XLFN_BENCH_MEASUREMENT_MS=3000 cargo bench -p xlfn --bench rtd_prepare \
  --features "bench-internals rtd" --locked -- \
  --warm-up-time 1 --sample-size 30 --save-baseline non-owning
```

### 2026-09-09 identity index comparison

A local macOS arm64 release comparison used the command above: 1 s warm-up,
3 s measurement, and 30 samples per case. The owning baseline was revision
`a65833b` with the same benchmark fixture copied into an isolated checkout.
The final non-owning implementation returns the matching entry alongside its ID
to avoid another entry lookup in `prepare`. These two measurements ran
sequentially after the workspace tests had finished.

Values are Criterion slope point estimates for one 256-subscription batch;
negative change means less time. This is one short local A/B run, not a Windows
or Excel performance qualification.

| Operation | Topic parts | Owning index | Non-owning index | Time change |
| --- | ---: | ---: | ---: | ---: |
| new | 1 | 84.38 µs | 57.48 µs | -31.9% |
| existing | 1 | 16.89 µs | 22.50 µs | +33.2% |
| churn | 1 | 113.89 µs | 70.26 µs | -38.3% |
| new | 10 | 206.91 µs | 58.86 µs | -71.6% |
| existing | 10 | 54.16 µs | 57.45 µs | +6.1% |
| churn | 10 | 378.57 µs | 103.67 µs | -72.6% |

New admission and churn improved in this run. Existing-topic reuse remained
slower, particularly for single-part topics, even after eliminating the
redundant entry lookup. The ownership change removes the retained topic copy
and insertion/removal deep clones, but does not establish a latency improvement
for every path. Confirm the reuse tradeoff with repeated full measurements on
the deployment host before setting performance thresholds. Allocation counts
and retained bytes were not measured by this timing fixture.

## Protocol costs

`protocol_costs` includes removal/final-drain and retirement-debt probes,
queue microbenchmarks, and optional real Rust RTD pipeline measurements:

```text
cargo bench -p xlfn --all-features --bench protocol_costs
cargo bench -p xlfn --all-features --bench protocol_costs -- --pipeline
cargo bench -p xlfn --all-features --bench rtd_publish -- channel_pipeline
cargo bench -p xlfn --all-features --bench rtd_refresh -- channel_pipeline
XLFN_TOPOLOGY_SUBSCRIPTIONS=512 XLFN_TOPOLOGY_PUBLISHERS=1 cargo bench -p xlfn --all-features --bench protocol_costs -- --topology
cargo bench -p xlfn --all-features --bench protocol_costs -- --token-cache
```

The pipeline includes RtdSender, publisher, ErasedSink, PublishCore, and refresh
planning/completion with sequence checks. Excel/COM is not timed. Shared
publisher topology and token-cache associativity remain benchmark-only
controls; normal builds retain per-subscription publishers and direct
mapping.
