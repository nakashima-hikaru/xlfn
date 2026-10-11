# Benchmark notes

## 2026-10-11 short borrowed strings

The first nonempty borrowed string in a call now uses an inline `SmolStr`
when its decoded UTF-8 length is at most 23 bytes. The slot stays unchanged
until call teardown, so later arguments cannot invalidate earlier borrows.
Further strings and large typed collections retain arena storage. Empty
strings return a static empty slice; public owned `String` input is unchanged.

Baseline `3cb4ed15df314e6cb409dfdef39134e6bb499b2d` and the candidate used
separate source/target directories, identical shared fixtures, frozen executable
hashes, and ABBA order on macOS ARM64 with Rust 1.99.0. Each run used 20 samples,
0.5 s warm-up, and 2 s measurement. Values are geometric means of two estimates
per side; individual confidence intervals are not intervals for the A/B
difference.

| Borrowed input | Baseline | Candidate | Time change |
| --- | ---: | ---: | ---: |
| Short ASCII | 72.4 ns | 44.5 ns | -38.5% |
| Short Unicode | 69.6 ns | 46.5 ns | -33.1% |
| ASCII, 23 UTF-8 bytes | 82.1 ns | 63.7 ns | -22.3% |
| Unicode, 23 UTF-8 bytes | 75.8 ns | 57.2 ns | -24.5% |
| Empty | 26.9 ns | 26.3 ns | -2.4% |
| Two strings in one call | 79.5 ns | 79.6 ns | +0.1% |

Short ASCII and Unicode now request zero allocations, compared with one
496-byte allocation per call. Strings above the inline limit retain one
allocation. Counts cover 10,000 warmed calls and describe allocator requests,
not RSS or peak live memory. The allocation gate asserts that the short
borrowed inputs remain allocation-free.

The 23 shared cases include 24-byte boundary controls, long text, 100/10k
string matrices, 100k mixed/numeric matrices, and eight-argument calls with
1/4/16/32 workers. Scalar numeric ingress rose 1.2% (23.1 to 23.4 ns), and
eight-argument batches with one worker rose 1.4%. The other long/bulk/concurrent
controls ranged from -1.3% to +0.7%; these small differences are not claimed
as statistically significant. An unchanged owned short-string control measured
9.9% faster; this is not attributed to the borrowed-string optimization.

The initial candidate slowed empty strings by 52.5%. It was revised to bypass
storage for empty strings, and the full ABBA comparison above was rerun.
[Results, both experiments, source patches, allocation logs, and provenance](results/2026-10-11-smol-input.json)
preserve all measured cases.
An additional ABBA run of the three-cell mixed borrowed fixture measured
-3.1% with overlapping per-run confidence intervals. Its 100-cell mixed
controls had wide baseline intervals and much larger gains, which are
retained in the record without claiming a general matrix improvement.

Validation: workspace nextest 1,232 passed / 11 skipped; all-target/all-feature
Clippy, strict workspace rustdoc, formatting, and panic-boundary checks passed.
The borrow-retention test passed Miri with strict provenance and with strict
provenance plus Tree Borrows. Compile-fail snapshots retain the original
thread/escape errors and now include the private inline slot's `Sync` note.
All 17 `just bench-ci` recipes passed, including the new boundary controls
and zero-allocation assertions. That gate ran alongside validation checks;
its timings are excluded from the isolated comparisons. Windows x86_64/i686
library checks passed with `blake3/pure`; the default backend required an
unavailable MSVC assembler (`ml64.exe`).

These local measurements do not establish native Windows/live Excel speed
or the remaining Excel-DNA performance gap.

## 2026-10-09 value boundaries

This change removes repeated scalar-return ownership work, adds an explicit
contiguous numeric input path, and avoids intermediate collection copies in
the Excel-comparison functions. The comparison's cells, formulas, polling,
and acceptance criteria are unchanged.

Primitive number, Boolean, and Excel-error returns reuse a process-lifetime
`XLOPER12` root per producing thread and loaded module. Each root requests
32 bytes, plus allocator overhead, and remains allocated after thread exit.
Windows uses the process heap so the root does not depend on a physically
unloaded DLL's allocator. These roots have no `xlbitDLLFree` ownership transfer;
strings and arrays retain their tracked ownership and cleanup. The producer
guard still protects code execution during publication. This relies on Excel
consuming the scalar before another call on the same thread replaces it.

`NumericMatrixRef` accepts Excel's `K%`/FP12 input as a borrowed contiguous
double slice. M01, M03, and M04 explicitly select that path; existing
`Matrix<f64>` inputs retain their strict conversion contract. FP12 coercion
can turn blank cells into zero and reject incompatible cells before Rust runs,
so it cannot preserve every raw cell distinction. Output arrays remain ordinary
`Q`/`XLOPER12` values, preserving error, panic, and closing-result behavior.
M02 fills `XlArrayBuilder` directly, and M03 uses `XlArrayBuilder::from_f64`
instead of constructing an intermediate owned matrix.

M05 borrows mixed cells through `MatrixRef<ExcelCellRef>`. After a collection
of at least 64 KiB, scratch storage allocates a separate text arena only if
text is actually decoded. Small collections and large numeric-only collections
keep their previous allocation strategy. Deliberately returned Excel errors
remain error outcomes for instrumentation but do not enter the framework's
failure-diagnostic stream. Other failures still do. Input-fingerprint errors
are now constructed only on the failing branch, avoiding normal-path error
construction and destruction for scalar arguments.

Baseline `dd621818ee5266f049314d08d3fef1b29ac3df2b` and the candidate used separate
source and target directories, common fixtures for shared cases, frozen
executables, and SHA-256 checks before/after each invocation. The ABBA run used
macOS ARM64, Rust 1.99.0, 20 samples, 0.3 s warm-up, and 1.5 s measurement.
The values below are geometric means of two Criterion estimates per side;
individual confidence intervals are not intervals for the A/B difference.

The following comparisons select the API paths adopted by the Excel fixture.
They compare different supported input/output paths rather than claiming that
every call using the previous type became faster.

| Path, 100k cells | Previous path | Adopted path | Time change |
| --- | ---: | ---: | ---: |
| Owned strict numeric input → borrowed FP12 input | 349.62 µs | 41.63 µs | -88.1% |
| Owned matrix construction/return → direct builder | 83.12 µs | 69.61 µs | -16.3% |
| Owned matrix conversion/return → numeric slice conversion | 65.40 µs | 60.18 µs | -8.0% |
| Mixed ASCII cells, owned → borrowed | 1186.60 µs | 806.42 µs | -32.0% |
| Mixed Unicode cells, owned → borrowed | 1283.11 µs | 903.67 µs | -29.6% |

Common scalar cases include admission, execution, return publication, and
cleanup. Each measured batch performs 1,000 calls per persistent worker;
worker dispatch/completion is included and worker startup is excluded.

| Common path | Workers | Baseline batch | Candidate batch | Time change |
| --- | ---: | ---: | ---: | ---: |
| Scalar return | 1 | 51.15 µs | 42.33 µs | -17.2% |
| Scalar return | 32 | 3317.72 µs | 2550.31 µs | -23.1% |
| Eight scalar arguments | 1 | 101.63 µs | 85.65 µs | -15.7% |
| Eight scalar arguments | 16 | 1413.13 µs | 1304.79 µs | -7.7% |
| Eight scalar arguments | 32 | 3827.98 µs | 3175.73 µs | -17.0% |
| One scalar argument | 16 | 1250.35 µs | 1368.49 µs | +9.4% |
| One scalar argument | 32 | 3086.97 µs | 3180.52 µs | +3.0% |
| Deliberate Excel error, no subscriber | 1 | 57.55 µs | 40.29 µs | -30.0% |
| Deliberate Excel error, no subscriber | 32 | 4093.83 µs | 2741.28 µs | -33.0% |
| Domain failure, no subscriber | 1 | 55.10 µs | 45.93 µs | -16.7% |
| Domain failure, no subscriber | 32 | 3508.11 µs | 3204.42 µs | -8.7% |

These common costs affect S01, P02, and W03, which use the same scalar UDF.
S02 retains strict scalar conversion. P04 retains its intentional shared
atomic contention, so removing scalar-return overhead does not remove that
workload's contention. Improvements vary with worker count and argument count:
the one-argument 16- and 32-worker regressions above remain in the result.
These short batches include coordination and do not establish a universal
multithreaded throughput improvement.

The 100k mixed borrowed fixture's requested allocation bytes per call fall
from 7,200,736 to 2,609,128, while allocations rise from two to four. Its owned
control requests 2,480,000 bytes in 20,001 allocations. The 1k borrowed case
remains at 73,696 bytes in two allocations. These are allocator-request counts
and cumulative requested bytes, not process RSS or live-memory measurements.
An eagerly separated text arena was rejected: it reduced large-input bytes
but slowed three-cell, 100-cell, and 100k-cell borrowed conversion substantially.

Other regressions remain visible. Short borrowed-string conversion rose 10.1%
(69.24 ns to 76.26 ns), 1k-string identity rose 5.6%, strict numeric matrix
conversion with identity rose 4.1%, and raw string-matrix identity rose 4.7%.
Borrowed 10k-string conversion rose 2.8%, and 100-cell mixed borrowed conversion
rose about 2.1%. Plain owned numeric matrix conversion rose 0.7%; its earlier
11.0% result is retained in the experiment record rather than discarded.

The initial candidate introduced an outlined generic byte copy for each
numeric identity value. Inlining the byte writer restored direct numeric
stores, but a longer 40-sample/5 s ABBA confirmation still found the warm
100k owned-matrix formula control 10.4% slower. A private `write_u64` function
boundary now keeps numeric writes fixed-width while separating buffer
management from their callers. That control improved 3.6% in a separate
30-sample/3 s experiment and 3.5% in the final paired run (733.16 µs to
707.27 µs). This does not establish an improvement for every identity path;
the strict numeric-conversion and string-identity regressions remain above.

[All shared/new cases, per-run estimates, allocation logs, source patches,
rejected experiment, and executable provenance](results/2026-10-09-value-boundary.json)
include the regressions as well as the adopted-path gains.

Validation: final-source workspace nextest 1,230 passed / 11 skipped.
All-target/all-feature Clippy, formatting, and panic-boundary checks passed
on the final source. Strict xlfn rustdoc and guide checks also passed.
Detached scalar roots passed strict provenance and Tree Borrows with their
intentional process-lifetime allocations; FP12 borrowing passed both modes,
and the lazy text-storage lifetime passed strict provenance. These Miri and
Windows x86_64/i686 library cross-checks preceded the final private writer
boundary; that change preserves the same little-endian bytes and ownership.
`just bench-ci` completed all 17 recipes with
`XLFN_BENCH_MEASUREMENT_MS=500`; fixed-timing groups retained their defaults.
This execution/allocation gate ran alongside the final workspace checks; its
timings are excluded from the isolated ABBA performance comparison.

These are local production-path measurements. They do not measure Excel's
FP12 coercion cost, the complete Excel-DNA gap, or native Windows/live Excel
performance. The Excel fixture still needs native qualification, including
the registered `K%` input path and process-lifetime scalar roots.

## 2026-10-06 plain input conversion

The `ExcelParameter<PlainInputMode>::decode` blanket implementation only
forwards to `T::from_excel`. In the baseline ARM64 release binary, this wrapper
remained an out-of-line call for every numeric array cell. Inlining the wrapper
lets the existing `f64::from_excel` implementation join the conversion loop.
Header validation, array budgets, error ordering, and allocation policy are
unchanged. The underlying custom converter retains its own inlining policy.

Baseline `3291518b9678a1934007bd7ed422b120ba59b6a5` and the candidate used separate
target directories, frozen executables, and SHA-256 checks before/after every
run. The unchanged `argument_ingress` fixture ran in ABBA order on macOS ARM64
with Rust 1.99.0, 0.5 s warm-up, 3 s measurement, and 30 samples per case. Values
below are medians of the two Criterion slope estimates on each side, not
confidence intervals for the A/B difference.

| Input | Baseline | Candidate | Time change |
| --- | ---: | ---: | ---: |
| `Matrix<f64>`, 100k cells, plain | 398.40 µs | 350.27 µs | -12.1% |
| `MatrixRef<f64>`, 100k cells, plain | 410.90 µs | 355.64 µs | -13.4% |
| `Vec<f64>`, 100k cells, plain | 402.53 µs | 347.53 µs | -13.7% |
| `MatrixRef<f64>`, 16 cells, plain | 131.18 ns | 114.72 ns | -12.5% |
| `Matrix<f64>`, 100k cells, with identity | 1063.53 µs | 1067.57 µs | +0.4% |
| `Matrix<String>`, 10k cells | 381.64 µs | 396.07 µs | +3.8% |
| Borrowed string matrix, 10k cells | 175.91 µs | 180.43 µs | +2.6% |
| Scalar `f64` | 22.51 ns | 23.05 ns | +2.4% |

[All 15 cases, individual estimates, source patch, and executable provenance](results/2026-10-06-plain-ingress.json)
include the small regressions as well as improvements. Do not extrapolate this
compiler-sensitive result to every input type. Splitting the array budget's
string branch also improved numeric input, but slowed identity conversion by
3.7% in screening and was reverted. Numeric-output inlining had no consistent
large-array benefit and was also reverted.

`just bench-ci` now includes plain owned/borrowed numeric matrix input and
owned string matrix input, alongside its identity and borrowed-string controls.
These are Excel-independent production-path measurements. They do not establish
the cause or size of the full Excel-DNA gap, and do not qualify Windows/Excel
performance. The event-registration fix made afterwards is outside this paired
source comparison.

Validation of the combined working tree (including the event-registration
repair): workspace nextest 1,165 passed / 11 skipped; comparison-runner Python
tests 67 passed; all-target/all-feature Clippy, strict xlfn rustdoc, formatting,
and panic-boundary checks passed. `just bench-ci` completed all 17 recipes with
`XLFN_BENCH_MEASUREMENT_MS=500` (groups with fixed timing retained their default).
That shortened gate checks execution and allocation assertions; its timings
are not part of the isolated ABBA comparison above. Registration tests also
passed without default features (36 tests).

The Windows XLL cross-check was blocked by missing `ml64.exe` on macOS. An
auxiliary x86_64 MSVC library check with `async,rtd,handles,cache,blake3/pure`
passed; it does not establish a native XLL build or Excel execution.

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
cargo bench -p xlfn --features bench-internals,async-builtin --bench value_boundary_allocations
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
