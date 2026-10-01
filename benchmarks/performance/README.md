# Large-array and handle-release measurements, 2026-09-30

The follow-up [2026-10-01 comparison](2026-10-01.md) measures deferred borrowed
numeric inputs and the remaining pin, cache-flight, and callback admission
coordination changes against the committed version of this implementation.

Numeric input preparation and identity encoding now stream validated cells into
the selected encoder sink. Object release now registers completion and updates
binding/pin counts under one lock. The release guard still completes under a
second lock, after user destruction and shutdown notifications. Ordinary
release therefore takes two arena mutex acquisitions instead of three; user
destructors remain outside the mutex.

The existing `XlArrayBuilder` is the recommended output path when a UDF is
constructing a large result directly. New benchmarks compare it with building
and returning a `Matrix<f64>`. The guide also shows how consumers can share a
producer's handle instead of repeatedly passing its large source range.

Concurrent cold-cache benchmarks were added without changing the resident
budget, sharding, or ownership policy. They expose coordination costs and are
included in CI and the full scaling suite. There is no blanket Arc replacement
or cache architecture change in this revision.

## Paired production comparison

The baseline is `c87ed9fcf44f572ead9e9a170ce3d69db48a0340`, the checkout's HEAD
before these changes, rather than the older `e9a8129` quoted in the request.
Both builds used Rust 1.98.1, release mode, macOS arm64, the system allocator,
SDK 15.2, and `bench-internals,async,cache`. Identical benchmark instrumentation
was added to the baseline using [baseline-harness.patch](baseline-harness.patch).
Compilation finished before timing; separate executables were frozen and
identified by SHA-256.

Criterion ran in baseline/candidate/candidate/baseline order, with 0.3 seconds
of warmup, 1 second of measurement and 50 samples per case in each round. The
table uses the median of each implementation's two round medians. Raw round
medians, means, confidence intervals and executable hashes are in
[2026-09-30-paired.json](2026-09-30-paired.json). These short local measurements
support a scoped adoption decision; the default CI measurement policy remains
10 seconds per case.

| Workload | Baseline | Candidate | Time change |
| --- | ---: | ---: | ---: |
| Prepare and identify 100k numeric cells | 1,117.1 µs | 986.4 µs | -11.7% |
| Identity for 100k numeric cells | 885.0 µs | 727.2 µs | -17.8% |
| Formula handle warm hit, 100k numeric cells | 886.4 µs | 734.6 µs | -17.1% |
| Convert and identify owned numeric Matrix, 100k cells | 1,215.0 µs | 1,189.2 µs | -2.1% |
| Convert and identify ExcelValue Matrix, 100k cells | 2,404.4 µs | 2,096.3 µs | -12.8% |
| Pin acquire/release, serial | 34.84 ns | 27.47 ns | -21.2% |
| Final pin release | 29.60 ns | 26.18 ns | -11.6% |
| Same object, 4 workers × 1,000 pin cycles | 269.3 µs | 209.1 µs | -22.4% |
| Distinct objects, 4 workers × 1,000 pin cycles | 274.2 µs | 227.8 µs | -16.9% |

Final-pin fixture creation and arena cleanup are excluded from timing. Its
release includes production final destruction. Persistent-worker batch times
include dispatch and completion coordination, with thread creation excluded.
Formula warm-hit rows start from already-owned Rust inputs and include identity
encoding and handle observation; Excel argument conversion is measured by
`argument_ingress` and the two-phase diagnostic below.

The change is not a universal speedup. Single-cell Matrix preparation rose
from 366.5 to 380.2 ns (+3.7%, about 14 ns), and plain 100k-cell numeric Matrix
conversion rose from 414.3 to 429.9 µs (+3.8%, about 16 µs). Scalar formula warm
hits rose 0.8%; borrowed 10k-string conversion rose 0.3%. All 29 paired cases,
including these results, are retained in the JSON. Large numeric identities
and pin workloads improved sufficiently to adopt the change for the stated
priorities, while retaining the small-input and plain-conversion regressions
as limits to revisit on the deployment host.

A trial that copied numeric encodings through a 512-byte temporary buffer was
rejected: numeric preparation became about 18% slower in its paired comparison.
[2026-09-30-rejected-buffer.json](2026-09-30-rejected-buffer.json) records that
result. The adopted implementation streams into the existing hash workspace
after selecting its sink once; it does not use that extra buffer.

## Input semantics and materialization

Every source cell still passes raw-cell validation, the input budget, and
numeric conversion before a formula cache lookup. The framing, little-endian
floating-point bits, inline-to-hashed boundary, signed zero, integer
canonicalization and error order are unchanged. A validation failure poisons
the encoder even if a caller ignores the immediate error. No source address,
range shape, or previously cached identity can substitute for checking cell
contents.

The existing prepared-input path avoids creating an owned Matrix on a warm
formula hit. A miss still materializes the input and validates during that
materialization. The new loop improves preparation; it does not eliminate
that second pass on a miss or make content hashing constant-time.

The separate `two_phase_ingress` diagnostic uses real formula publication and
custom batch timing rather than Criterion. Its recorded 100k-cell prepared
path improved from baseline to candidate in all three phases:

| Prepared formula phase | Baseline | Candidate |
| --- | ---: | ---: |
| Warm | 1,098.4 µs | 986.9 µs |
| Cold | 1,328.2 µs | 1,218.5 µs |
| Changed last cell | 1,326.9 µs | 1,218.2 µs |

Within the candidate build, warm preparation used two allocation requests
and 6,106 requested bytes per call, versus three requests and 806,106 bytes
for eager conversion. The cold prepared path remained slower than eager
conversion (1,218.5 versus 1,111.4 µs), an existing cost of validating before
lookup and materializing on a miss. Both cold paths requested 806,941 bytes.
These figures are diagnostic batch summaries, not additional Criterion
confidence estimates. For repeated large-data consumption, sharing one
producer handle moves the content scan to the producer's recalculation.

## Direct numeric output

These comparisons use two existing APIs in the candidate build, rather than
a production before/after change. Both construction paths compute identical
cells and include return admission, publication and free. Full cell
equivalence is checked outside timing. Two measured rounds used the same
Criterion policy as above.

| Workload | 1,000 cells | 100,000 cells |
| --- | ---: | ---: |
| Build and return `Matrix<f64>` | 867.0 ns | 82.62 µs |
| Build and return `XlArrayBuilder` | 673.8 ns | 62.65 µs |
| Convert and return an existing Matrix | 774.3 ns | 65.52 µs |

For 100k cells, direct construction was 24.2% faster than Matrix construction
plus return. The separate existing-Matrix row excludes fixture construction;
it should not be used as a claim about end-to-end result generation.

The independent allocation probe measured 100 calls per numeric output case:
building and returning a 100k-cell Matrix required two allocation requests
and 4,000,000 requested bytes per call, while direct construction required
one request and 3,200,000 bytes. It removes the 800,000-byte intermediate
numeric buffer. Requested bytes measure allocation traffic, not peak live
memory or RSS. Existing warm scalar/handle probes still report zero
allocations, and eight large identity arguments reuse one hash-workspace
allocation per call, matching one large argument.

## Concurrent cold cache

`cache_miss_concurrency` uses 1, 4, 16 and 32 persistent workers, 256 requests
per worker, one fixed 1 MiB resident budget, and the default Quick Cache
configuration (one shard). Clearing, start-channel sends and correctness probes
are outside the timed region; the start barrier and completion coordination
are inside it. Distinct
keys produce one miss per request. Same-key batches share a key for each
request index and include pending singleflight followers as well as resident
hits by later arrivals.

| Workload, batch median | 1 worker | 4 workers | 16 workers | 32 workers |
| --- | ---: | ---: | ---: | ---: |
| Distinct cheap misses | 51.7 µs | 423.1 µs | 3.220 ms | 7.829 ms |
| Cheap computation only | 2.8 µs | 19.0 µs | 51.7 µs | 93.5 µs |
| Dispatch only | 2.8 µs | 19.1 µs | 52.2 µs | 91.6 µs |
| Distinct misses, 4,096-number reduction | 1.033 ms | 1.204 ms | 6.701 ms | 18.376 ms |
| Numeric computation only | 0.964 ms | 1.064 ms | 3.014 ms | 5.547 ms |
| Same-key numeric reuse | 1.032 ms | 1.382 ms | 2.070 ms | 2.649 ms |

Batch request counts grow with the worker count; these are not per-request
latencies. The compute-only and dispatch-only rows are controls with the same
worker topology. The cache adds substantial coordination cost for cheap
misses, and numeric distinct misses also lose scaling at high worker counts.
This benchmark does not isolate the flight mutex from index, admission,
allocation, reclamation, or scheduling costs. It therefore establishes a
measurement target rather than attributing all delay to one lock. No shard
increase was adopted: it would alter capacity policy and potentially hit rate
without an isolated comparison or Windows evidence.

Untimed probes check result checksums, exact compute counts, repeat resident
hits, entries/weight within budget, and zero reclamation debt after clear.
All 36 cases completed in one measured round. Output, cache, allocation and
two-phase results are retained in
[2026-09-30-output-cache.json](2026-09-30-output-cache.json).

## Validation and limits

[2026-09-30-validation.json](2026-09-30-validation.json) records the checks:

- Workspace Clippy for all targets/features passed.
- Process-isolated nextest: 1,035 passed, 11 skipped.
- Final serialized xlfn libtests: 758 passed, 10 ignored. The earlier workspace
  serialized/doc-test run passed before the final encoder-loop adjustment.
- All 48 feature combinations compiled before the final internal loop
  adjustment; its interface did not change.
- Strict-provenance Miri with Stacked and Tree Borrows each passed five
  prepared-input tests, two numeric-encoder tests, and one final-release test.
- Release tests cover destructor lock reentry, nested releases, quiescence
  rejection during release, panic containment and final zero counters. Native
  mutex/destructor composition remains outside the existing formal proof.
- Sleep-based zero-budget and overweight singleflight fixtures now witness
  pending followers. All three singleflight tests passed 100 repetitions each;
  production cache behavior was unchanged.
- The panic-boundary audit reviewed 50 boundaries; guide validation checked
  18 chapters and compiled the values/handles examples.

The new output, pin-release and selected cache-miss cases run in `just bench-ci`.
`just bench-scaling` runs the full pin/cache concurrency curves, and
`just bench-full` includes all new benches. No version, publication, ABI or
public production API change is required.

These are Excel-independent macOS measurements. No new native Windows run,
live Excel UDF latency, or valid Excel-DNA comparison was performed. The
previous Windows numbers cannot be directly compared with this host's values.

## Reproduction

Run from the repository root with Rust 1.98.1. For the recorded macOS setup,
use SDK 15.2; on another host choose its native toolchain/SDK and treat the
results as a separate measurement. Start with a fresh output directory.

```sh
rtk proxy mkdir -p target/performance-repro/baseline
rtk proxy sh -c 'git archive c87ed9fcf44f572ead9e9a170ce3d69db48a0340 | tar -x -C target/performance-repro/baseline'
rtk proxy git -C target/performance-repro/baseline apply ../../../benchmarks/performance/baseline-harness.patch
rtk proxy sh -c 'cd target/performance-repro/baseline && SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX15.2.sdk CARGO_TARGET_DIR=../baseline-build cargo bench -p xlfn --features bench-internals,async,cache --bench input_identity --bench argument_ingress --bench object_lease --bench formula_revision --no-run --locked --message-format=json > ../baseline-build.jsonl'
rtk proxy sh -c 'SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX15.2.sdk CARGO_TARGET_DIR=target/performance-repro/candidate-build cargo bench -p xlfn --features bench-internals,async,cache --bench input_identity --bench argument_ingress --bench object_lease --bench formula_revision --no-run --locked --message-format=json > target/performance-repro/candidate-build.jsonl'
rtk proxy python3 benchmarks/performance/compare.py \
  --baseline-build target/performance-repro/baseline-build.jsonl \
  --candidate-build target/performance-repro/candidate-build.jsonl \
  --work-dir target/performance-repro/paired \
  --output target/performance-repro/paired.json \
  --baseline-revision c87ed9fcf44f572ead9e9a170ce3d69db48a0340
```

For the output and cache curves, run each of the following. Their standard
policy is 10 seconds per case. To reproduce the short exploration, set
`XLFN_BENCH_MEASUREMENT_MS=1000` and pass
`-- --noplot --warm-up-time 0.3 --sample-size 50`; run the output comparison
twice. Set `SDKROOT` as above when using the recorded macOS installation.

```sh
rtk cargo bench -p xlfn --features bench-internals,async,cache --bench array_numeric_output --locked
rtk cargo bench -p xlfn --features bench-internals,cache --bench cache_miss_concurrency --locked
rtk cargo bench -p xlfn --features bench-internals,async --bench value_boundary_allocations --locked
rtk cargo bench -p xlfn --features bench-internals --bench two_phase_ingress --locked
```

The last two are independent probes, so their results must remain separate
from Criterion timing. `two_phase_ingress` labels its eager/prepared variants
with historical `candidate=false/true` fields; within both recorded builds,
`candidate=true` means that build's production prepared-input path.
