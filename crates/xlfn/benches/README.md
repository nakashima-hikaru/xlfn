# Benchmarks

Run commands from the repository root. [Justfile](../../../Justfile) defines
the maintained CI and scaling suites; [Cargo.toml](../Cargo.toml) registers
every runnable target and its required features.

```sh
rtk just bench-ci       # Canonical regression suite; bench-pr uses the same suite.
rtk just bench-scaling  # Worker, batch, and input-size curves.
rtk just bench-full     # Unfiltered runs of the targets selected in Justfile.
rtk just bench-check    # Clippy for every benchmark with all features.
```

`bench-full` runs the selected targets without Criterion name filters.
Additional targets are available for focused investigations:

| Targets | Measurement | Features for `bench-one` |
| --- | --- | --- |
| `async_admission`, `async_admission_concurrent`, `async_admission_allocations` | Admitted/saturated async calls, concurrent launch/drain, and caller allocations | `bench-internals async-builtin` |
| `async_handle_scoped`, `async_handle_scoped_allocations` | Actual handle-scoped task delivery and caller allocations | `bench-internals async-builtin` |
| `cache_lookup`, `cache_workloads` | Warm lookup/live-lease retirement and deterministic weighted recalculation traces | `bench-internals cache` |
| `diagnostics_queue`, `diagnostics_queue_allocations` | Reporting, backpressure, opt-in subscribers, and retained payloads | `bench-internals` |
| `handle_memory` | Requested live bytes during sparse growth and publication churn | `bench-internals` |
| `input_identity` | UTF-16 and scalar/matrix identity encoding | `bench-internals` |
| `protocol_costs` | Handle retirement, token cache, queues, and RTD pipeline probes | `bench-internals rtd` |
| `two_phase_ingress` | Prepared/eager input through actual formula publication, with time and allocation JSON | `bench-internals` |

For example:

```sh
rtk just bench-one async_admission "bench-internals async-builtin"
rtk just bench-one input_identity
```

Criterion groups normally use a ten-second measurement period.
`XLFN_BENCH_MEASUREMENT_MS` overrides measurement time for short local runs;
Criterion warmup and sample-count arguments are separate. Custom JSON and
allocation probes use their own iteration counts.

`rtk just bench-native-abi` compares strict `Q` with native `B`, `A`, `J`,
and `D%` admission using identical call setup in one executable. The inputs
cover 1/2/4/8 numbers, Boolean, i32, and short/long Unicode. Each pair measures
plain conversion, prepared input identity without materialization, and full
identity plus materialization. The warm preparation rows model a handle
producer's input phase; they do not time handle lookup or publication.
Representative pairs also run in `bench-ci`. Excel preprocessing and actual
worksheet recalculation are measured separately by
[`benchmarks/excel-comparison`](../../../benchmarks/excel-comparison/README.md).

These benchmarks execute Rust paths independently of Excel/COM. Allocation
traffic, requested live bytes, timing, and production-feature measurements
have different scopes, described below. For A/B comparisons, use identical
fixtures and feature selections, separate source/target directories, frozen
executable hashes, and repeated ABBA/BAAB ordering.

## Handle object working sets

`handle_lookup` covers one shared token, one fixed token per worker, and
`rotating_65536_tokens` with 65,536 live objects and 1/4/16/32 workers. Rotating
workers advance through disjoint strides and retain their cursor across timed
batches; the benchmark does not repeatedly read only the first batch of tokens.
Every lookup includes the call scope, token validation, and typed projection.
Population, thread creation, and teardown stay outside timing, while batch
dispatch and completion remain included.

`handle_prepare/cold_grow` measures publication at scale. Its `drop/1000` and
`drop/10000` cases populate outside timing and measure final runtime destruction.
`handle_memory` separately reports requested live allocation bytes rather than
allocator usable sizes or process RSS.

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

The `cache_nonresident/roundtrip` and `cache_deferred_queue` groups add
scalar roundtrip and key-ownership controls. To measure the ordinary production
configuration without benchmark observation counters:

```sh
rtk cargo bench -p xlfn --bench cache_reclamation --no-default-features --features cache --locked
```

Observation fields unavailable in this configuration are JSON `null`.
Enable `bench-internals,cache` for the observation metrics described above.

Run the normal ten-second-per-case measurements with 256 operations per worker
and 20 flat samples:

```text
cargo bench -p xlfn --bench cache_reclamation --features bench-internals,cache
```

For a short execution/metrics smoke check of the matrix and controls, use 64
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

The target also measures admission rejection, existing-topic reuse, and new
insertion/rollback with prevalidated 16 KiB and 1 MiB topics. The
`rtd_subscribe_input` group includes the public subscription input path for
256 or 4,096 topics. Run every group with:

```sh
rtk just bench-one rtd_prepare "bench-internals rtd"
```

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
cargo bench -p xlfn --features bench-internals,async-builtin --bench value_boundary_allocations
```

Each case warms up 100 calls and normally measures 10,000 calls through the system
allocator. Optional numeric collection probes use five measured calls at 1,000
and 100,000 cells; their preparation/discard counts must equal the corresponding
`Matrix<f64>` control. They cover owned `Option<f64>`, owned
`OptionalExcelValue<f64>`, and borrowed `Option<f64>` matrices. Numeric output
probes use 100 calls. Enum and borrowed-string cases each construct and drop a 1,000-cell
array containing the same `Ready` labels. The probe requires their allocation
counts and requested bytes to match. Warm handle inputs, with and without input
identity, must allocate nothing; the `async` feature additionally checks pending
handle conversion. Requested bytes are cumulative allocation traffic, not live
memory or RSS. This fixture does not measure elapsed time or Excel execution.
With `refinement` enabled the runtime rows report their counts but explicitly
skip zero-allocation assertions, since trace recording may allocate. Run the
command above without `refinement` for the allocation regression gate.

`rtd_prepare_allocations` separately requires zero caller-thread allocations for
prevalidated borrowed topics rejected by pending count, topic-byte or source-ID
limits, and for existing-topic reuse. It covers 16 KiB and 1 MiB topics with eight
warm-up calls and one measured call. Topic validation, fixture construction and
runtime teardown are excluded. `rtd_prepare` times the same admission paths plus
successful new-topic insertion and rollback. Both probes are in `just bench-ci`
and `just bench-full`. `XLFN_BENCH_REPORT_ONLY=1` disables the new optional/RTD
allocation assertions only when measuring a historical baseline; the canonical
gate leaves it unset.

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
The slice-copy probes cover 100, 1,000, 10,000 and 100,000 cells:
`numeric_slice_convert_return` returns FP12, while
`numeric_slice_convert_return_q` uses `XlArrayBuilder::from_f64` and Q, matching
the output path of the live Excel M03 fixture. M02 instead uses the separate
`push_f64` producer. Improvements to the FP12 slice constructor do not establish
improvements to either live M02 or M03.
`cache_miss_concurrency` keeps one 1 MiB resident budget and reports cold batches at
1, 4, 16, and 32 workers, plus compute/dispatch controls and untimed correctness
probes. Same-key reuse combines pending followers and resident hits.

Selected cases run in `just bench-ci`; full concurrency curves run in
`just bench-scaling`. These are Excel-independent benchmarks.

## Excel string comparison probes

`argument_ingress` includes owned ASCII and Japanese 4,096-character inputs.
`array_string_output` includes ASCII and Japanese 8/64/512/4,096-character scalar
outputs, plus ASCII/Japanese/mixed string copies. The short probes cover the
T02/T03 sizes where the live Excel comparison showed the largest gaps.
The copy probes include strict UTF-16
input decoding, return encoding, synchronous publication and owning cleanup;
the prebuilt UTF-16 input is outside the timer. They do not include Excel/COM
or formula-entry overhead. These cases complement short strings, mixed BMP and
astral text, empty strings, large string matrices and numeric controls.
