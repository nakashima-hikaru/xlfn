# Benchmark notes

## `async_task_drain`

This production-path benchmark drains 0, 1, 4, 5, 32, or 128 task controls
from all 32 generation shards, then drops the controls outside the shard locks.
Fixture construction, cancellation slot allocation, and fixture destruction
are excluded from timing. It does not measure normal task spawning or full
executor shutdown. The group follows the standard ten-second policy and
supports `XLFN_BENCH_MEASUREMENT_MS` for local comparisons.

`async_task_drain_allocations` separately warms cancellation slot recycling,
then counts only allocator traffic during drain and control destruction.
It requires zero allocations for 0–4 controls and exactly one allocation with
no reallocations for the larger cases. The allocation probe is in
`just bench-ci`; both targets are in `just bench-full`.

```sh
rtk cargo bench -p xlfn --features bench-internals,async --bench async_task_drain --locked
rtk cargo bench -p xlfn --features bench-internals,async --bench async_task_drain_allocations --locked
```

## `cache_lookup`

This benchmark measures production `CalculationCache<u64, u64>` lookup and
lease release. It covers one warm key, one shared hot key, and disjoint keys
with 1, 2, 8, and 32 persistent workers, plus live-lease retirement with a
one-entry resident budget. Fixture creation and worker warmup are outside
timing; dispatch and completion coordination remain in each measured batch.
Allocation counts are measured separately after warmup.

```sh
rtk cargo bench -p xlfn --bench cache_lookup --features bench-internals,cache --locked
```

## `cache_reclamation`

This benchmark measures eviction/reclamation through the ordinary
`CalculationCache::get_or_try_insert_with` and `CacheLease::drop` APIs. It does
not call `len`, `used_weight`, or `clear` in measured workloads. Every
operation uses a fresh, worker-disjoint key.

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
  reads counters and cannot advance index maintenance or a grace period.
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
cargo bench -p xlfn --bench cache_reclamation --features bench-internals,cache
```

For a short execution/metrics smoke check of all twelve cases, use 64
operations per worker and Criterion's test mode:

```text
XLFN_CACHE_RECLAIM_OPERATIONS=64 cargo bench -p xlfn --bench cache_reclamation \
  --features bench-internals,cache -- --test
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
cargo bench -p xlfn --all-features --bench protocol_costs -- --token-cache
```

The pipeline includes RtdSender, publisher, ErasedSink, PublishCore, and refresh
planning/completion with sequence checks. Excel/COM is not timed. The
token-cache probe measures the production 16-set, four-way token cache with
colliding tokens and registry switches.

## Performance evaluation

The benchmarks cover public cache resolution, input identity, output conversion
and handle ownership. `handle_memory` instruments allocation separately from
timing.
`handle_prepare/revision_churn` measures warm re-observation;
`handle_prepare/republish` withdraws and recreates topics to measure publication
churn.

## Value boundary allocations

`value_boundary_allocations` measures allocation traffic separately from timing:

```text
cargo bench -p xlfn --features bench-internals,async --bench value_boundary_allocations
```

Each case warms up 100 calls and measures 10,000 calls through the system
allocator. Enum and borrowed-string cases each construct and drop a 1,000-cell
array containing the same `Ready` labels. The probe requires their allocation
counts and requested bytes to match. Warm handle inputs, with and without input
identity, must allocate nothing; the `async` feature additionally checks pending
handle conversion. Requested bytes are cumulative allocation traffic, not live
memory or RSS. This fixture does not measure elapsed time or Excel execution.
With `refinement` enabled the runtime rows report their counts but explicitly
skip zero-allocation assertions, since trace recording may allocate. Run the
command above without `refinement` for the allocation regression gate.

## Public cache registry

`cache_registry` measures `CacheRegistry::get` through a shared `CacheRegistry`,
including endpoint resolution and lease release. `distinct_endpoints` uses
1/8/32 persistent workers with a separate endpoint per worker. `endpoint_cycle`
uses one worker cycling through 1/8/16 endpoints to expose lookup cost when
switching endpoints. The bounded resolution cache checks at most two entries
in one of 32 sets; collisions fall back to the registry.
`same_id_marker_cycle` cycles through three or eight marker types sharing one
static ID, using statically dispatched calls. `same_address_prefix_cycle` cycles
through three slices with the same starting address and different lengths.
These cases detect selectors that omit type or length metadata.

Each batch contains 10,000 lookups per worker, except the three-marker case
which completes 3,333 whole cycles (9,999 lookups). Throughput uses the actual
lookup count. Setup, endpoint seeding, value assertions and worker warmup are
outside measurement; worker coordination remains inside each batch.

```text
cargo bench -p xlfn --features bench-internals,cache --bench cache_registry --locked
```

Both `cache_registry` and `value_boundary_allocations` are included in
`just bench-ci` and `just bench-full`.

## Two-phase raw ingress

`two_phase_ingress` compares production `ArgumentContext::prepare` with eager
decoding through the real formula handle lookup/publication path. It covers
scalar, 1k/100k numeric matrices and 10k Unicode string matrices, with warm,
cold and changed-last-cell cases. It emits timing and allocation JSON
independently of Criterion. These measurements exclude Excel/COM execution.

## Semantic identity regression fixtures

`input_identity` includes short/long ASCII and Unicode UTF-16, sparse Unicode,
the Excel string limit, and one/eight numeric matrix arguments. Input storage
is prepared outside timing. `value_boundary_allocations` checks that eight
large arguments reuse the same number of hash allocations as one argument;
it also reports one-string/rest-numeric arrays to expose speculative arena
reservation costs.

## Large arrays, handle pins, and concurrent cache misses

`argument_ingress/*/prepare_identity` stops after complete validation and identity
recording, matching the input phase of a warm formula-handle hit. `two_phase_ingress`
adds real formula publication and separately probes warm, cold, and changed inputs.

`object_lease` measures the production arena's pin admission/release with serial
and persistent-worker cases; final-pin setup and arena cleanup are outside timing.
`array_numeric_output` includes result construction, return publication and free;
`matrix_convert_return` excludes construction of the existing owned Matrix.
`cache_miss_concurrency` keeps one 1 MiB resident budget and reports cold batches at
1, 4, 16, and 32 workers, plus compute/dispatch controls and untimed correctness
probes. Same-key reuse combines pending followers and resident hits.

Selected cases run in `just bench-ci`; full concurrency curves run in
`just bench-scaling`. These are Excel-independent benchmarks.
