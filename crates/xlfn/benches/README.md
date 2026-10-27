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
| `async_handle_scoped`, `async_handle_scoped_allocations` | Actual handle-scoped task delivery and caller/process-wide allocations | `bench-internals async-builtin` |
| `async_pending_allocations` | Process-wide pending/reschedule/cancellation allocations and retained slot size | `bench-internals async-builtin` |
| `cache_lookup`, `cache_workloads` | Warm lookup/live-lease retirement and deterministic weighted recalculation traces | `bench-internals cache` |
| `diagnostics_queue`, `diagnostics_queue_allocations` | Reporting, backpressure, opt-in subscribers, and retained payloads | `bench-internals` |
| `handle_memory` | Requested live bytes during sparse growth and publication churn | `bench-internals` |
| `input_identity` | UTF-16 and scalar/matrix identity encoding | `bench-internals` |
| `protocol_costs` | Handle retirement, token cache, queues, and RTD pipeline probes | `bench-internals rtd` |
| `rtd_channel_allocations` | Public channel-source construction allocation counts, without subscriptions or workers | `rtd` |
| `rtd_connection`, `rtd_connection_allocations` | Registered-source subscription storage and full connect/disconnect lifecycle | `bench-internals rtd` |
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

## Registration preparation and host-call overhead

`registration` measures descriptor validation/preparation, a prepared registration
loop, and their combined cost at 10, 100, 1,000 and 5,000 functions. The host stub
returns an unbound name followed by a registration ID. Setup verifies one module
lookup, one conflict check and one registration per function, and cleanup of
every callback result. The timed loop preserves these production operations.
Real Excel name lookup/registration latency, DLL loading and add-in initialization
are outside this benchmark; use C02 in the Excel comparison suite for those costs.
`value_boundary_allocations` additionally reports preparation and combined
registration allocation counts at 10 and 5,000 functions.

The same allocation probe covers short ASCII/Japanese scalar returns and copies,
including the owned UTF-8 input and the retained UTF-16 return payload. It warms
the return-block slot before counting and immediately frees each owning return.
It does not measure multiple simultaneously outstanding returns or process RSS.

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

`async_handle_scoped_allocations` measures warmed scalar handle deliveries
with and without instrumentation. It reports caller-thread and process-wide
allocation requests for 1, 4, and 32 sequential tasks after 256 warmup tasks.
The process counters include the worker and require zero additional worker
allocations and zero reallocations. Requested bytes are cumulative allocation
traffic, not live memory or RSS; the fixture uses an ABI-probe callback.

`async_pending_allocations` covers the complementary pending path with one
poller: each of 1/4/32 tasks returns `Pending` four times, then either completes
or waits for calculation cancellation. It checks exact user poll and destructor
counts and measures process-wide allocation traffic, including the worker.
Pool/task-shard warmup, fixture creation, and final shutdown are outside counting.
The 1/4-task cases require one executor allocation per task. The 32-task cases
also include queue-block allocation and, on cancellation, one drained-control
batch; their budget rejects an additional allocation per task while allowing
those batch costs. No reallocations are allowed. The probe reports the reusable
cancellation slot's retained size separately because
embedding its task Waker changes pool memory independently of per-task traffic.
`XLFN_BENCH_REPORT_ONLY=1` disables the allocation gate for historical baselines.

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

`cache_reclamation_owned_key/churn` exercises the same production path with
owned, fixed-width `String` keys, 64-byte payloads, and 1/4/16/32 persistent
workers. Key construction, cloning, and destruction belong to each operation.
The payload records the complete key serial so each lease verifies the lookup
result. This group covers deferred key retirement, which the `u64` matrix does
not exercise. `cache_owner_allocation_probe` separately reports constructor
allocation requests for `u64` and `String` keys; owner destruction is excluded.

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

## RTD channel source allocations

`rtd_channel_allocations` separately constructs the public `RtdChannelSource`
with only the ordinary `rtd` feature. It measures 1, 4, and 32 retained sources
after 16 warmup constructions, both with default settings and after selecting
latest-value delivery, a producer limit, and a subscription limit. Public getters
verify these settings after construction. Retention-vector allocation, source
destruction, configuration checks, and JSON reporting occur outside counting.
The factory must remain uncalled; this probe creates no subscriptions or
producer/publisher workers. Each source must request one allocation and no
reallocations. Requested bytes are allocation traffic, not live memory or RSS.

```sh
cargo bench -p xlfn --no-default-features --features rtd --bench rtd_channel_allocations --locked
```

For a historical source tree, copy the same probe and bench declaration, then
set `XLFN_BENCH_REPORT_ONLY=1` to report counts without the new allocation gate.
Keep source code, feature selection, compiler, and executable hashes with the
baseline and candidate output. The normal regression gate leaves it unset.

## RTD subscription storage and connection lifecycle

`rtd_connection` times warmed batches of 1, 32, and 256 subscriptions. Every
batch connects all subscriptions before disconnecting any of them. `adapter`
calls the actual registered-source adapter; `runtime` additionally includes
connection publication, catalog transitions, and synchronous disconnection.
Both use a nonallocating source returning a nonzero-sized subscription, so a
per-subscription `Box` cannot disappear through zero-sized allocation elision.

Fixture construction, source registration, canonical-topic preparation, driver
storage, worker startup, and final runtime teardown stay outside timing. A
preparation guard retains each canonical catalog entry across reconnects.
The callback dispatch in the benchmark driver occurs once per whole batch.
These measurements do not call Excel or COM or start producer workers.

`rtd_connection_allocations` counts process-wide allocator requests separately
from timing. Warm adapter batches must allocate nothing; warm runtime batches
may allocate only one canonical-topic snapshot per connection. Reallocations
are forbidden in both warmed cases. Cold batches report source-storage growth,
map growth, and (for runtime mode) cleanup-worker startup without an allocation
gate. Requested bytes are cumulative allocation traffic. The retained-byte
delta subtracts deallocated layouts and replaced realloc layouts during the
batch; it exposes reusable storage retained after a cold high-water mark.
Neither measure is process RSS. Set `XLFN_BENCH_REPORT_ONLY=1` only to measure a
historical implementation without the warmed allocation assertions.

The `adapter_owner_lifecycle` cases count complete runtime/source construction,
one batch and final destruction after warming process/TLS infrastructure. They
require a zero requested-live-byte delta, including reclamation of all source
slot capacity at owner teardown.

```sh
cargo bench -p xlfn --features bench-internals,rtd --bench rtd_connection --locked
cargo bench -p xlfn --features bench-internals,rtd --bench rtd_connection_allocations --locked
```

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

`rtd_publish/channel_pipeline` covers 1, 4, 16, and 32 persistent sender
threads with queue capacities of 64 and 1,024. Each cycle drains through the
real shared publisher and verifies the final refresh marker and sequence count.
Worker startup and subscription setup remain outside timing.

The pipeline includes RtdSender, publisher, PublishSink, PublishCore, and refresh
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

`async_spawn` includes 1/4/8/16/32 executor workers and 1/4/16/32 producer
threads in its spawn, reschedule, and spawn-and-drain matrices. Use matching
worker/producer cases when comparing cancellation or scheduler changes; the
spawn-and-drain group includes completion of the admitted tasks.

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
