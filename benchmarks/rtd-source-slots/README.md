# RTD source-local subscription storage

The baseline is commit `34f32722ab49802f28617615a47d1b61948b97f7` plus
the connection benchmark harness. The candidate replaces one boxed erased
subscription per connection with reusable typed slots in each registered source.
Both use Rust 1.99.1 (`58cf8f9c3`, LLVM 23.1.1), optimized bench profile,
`bench-internals,rtd`, on aarch64 macOS. This is a native backend measurement;
Excel, Windows COM, producer work, source I/O and publication are excluded.

The source itself allocates nothing and returns an eight-byte subscription that
retains no sink and owns no worker. This nonzero size ensures the baseline's
subscription box requires an actual allocation. Each batch keeps 1, 32 or 256
subscriptions live simultaneously before disconnecting them. Driver storage,
registration, topic preparation and final fixture destruction are excluded.

`adapter` calls the actual registered-source adapter and cancellation/barrier
methods. `runtime` additionally calls connect, commit and synchronous disconnect
through the server. A preparation guard preserves canonical catalog entries
between batches. Warm measurements recycle one full batch first; cold
measurements include source storage/map growth and runtime cleanup-worker startup.

## Allocations

The process-wide allocator probe counts allocation requests, reallocations,
requested bytes and the requested-live-byte change. The last metric subtracts
deallocated layouts and old realloc layouts during the measured batch. It does
not describe process RSS or allocator-reserved memory. Raw rows are preserved in
[baseline-allocations.jsonl](baseline-allocations.jsonl) and
[candidate-allocations.jsonl](candidate-allocations.jsonl).

Warm batches:

| Path | Connections | Baseline alloc / realloc / requested B | Candidate alloc / realloc / requested B |
| --- | ---: | ---: | ---: |
| Adapter | 1 | 1 / 0 / 8 | 0 / 0 / 0 |
| Adapter | 32 | 32 / 0 / 256 | 0 / 0 / 0 |
| Adapter | 256 | 256 / 0 / 2048 | 0 / 0 / 0 |
| Runtime | 1 | 2 / 0 / 32 | 1 / 0 / 24 |
| Runtime | 32 | 64 / 0 / 1024 | 32 / 0 / 768 |
| Runtime | 256 | 512 / 0 / 8192 | 256 / 0 / 6144 |

Every warm batch has zero requested-live-byte change. The remaining runtime
allocation is its owned canonical-topic snapshot. The allocation regression
gate requires zero adapter allocations and exactly one runtime allocation per
connection, with no reallocations.

Cold high-water batches:

| Path | Connections | Baseline alloc / realloc / requested B / retained delta B | Candidate alloc / realloc / requested B / retained delta B |
| --- | ---: | ---: | ---: |
| Adapter | 1 | 1 / 0 / 8 / 0 | 1 / 0 / 96 / 96 |
| Adapter | 32 | 32 / 0 / 256 / 0 | 1 / 3 / 1440 / 768 |
| Adapter | 256 | 256 / 0 / 2048 / 0 | 1 / 6 / 12192 / 6144 |
| Runtime | 1 | 20 / 1 / 3518 / 3443 | 20 / 1 / 3734 / 3667 |
| Runtime | 32 | 145 / 4 / 32270 / 26055 | 114 / 7 / 36398 / 28359 |
| Runtime | 256 | 724 / 7 / 232358 / 121991 | 469 / 13 / 266950 / 140423 |

Reusable slots exchange repeated allocation for retained source capacity and
larger connection tickets. Cold allocation traffic and retained memory increase
despite fewer allocation calls. The cold adapter retains 96, 768 and 6144 bytes
after the measured batch; this capacity remains reusable until source teardown.
Cold runtime deltas also include cleanup infrastructure and connection maps.
Each source retains its own historical high-water capacity until unload. The
sum of these retained capacities across sources is not bounded by the global
`max_active` live-subscription limit when different sources peak at different
times. This one-source probe does not establish a global retained-memory cap.

The `adapter_owner_lifecycle` rows include the complete fixture/source/runtime
construction, one cold batch and final destruction, after warming process/TLS
infrastructure outside the counters. All three candidate and baseline cases
return to a zero requested-live-byte delta. The probe requires that zero delta
for the candidate, verifying that source teardown reclaims its high-water slot
capacity.

Reproduce the candidate gate with:

```sh
cargo bench -p xlfn --features bench-internals,rtd --bench rtd_connection_allocations --locked
```

Set `XLFN_BENCH_REPORT_ONLY=1` for the baseline. This bypasses only allocation
assertions, preserving the same measured work and counters.

## Timing

Two trials per revision ran in whole-process A-B-B-A order, with no concurrent
team build, test or Miri process. Every case used one second of warmup, two
seconds of measurement and 50 Criterion samples. The table averages the two
Criterion regression-slope point estimates, divided by connections per batch.
Throughput is the reciprocal of that average. Allocation counting uses a
separate executable and does not add allocator counters to these timing runs.

| Path | Connections | Baseline ns / connection | Candidate ns / connection | Time change | Throughput M connections/s, baseline → candidate |
| --- | ---: | ---: | ---: | ---: | ---: |
| Adapter | 1 | 30.87 | 52.85 | +71.2% | 32.39 → 18.92 |
| Adapter | 32 | 20.96 | 51.52 | +145.7% | 47.70 → 19.41 |
| Adapter | 256 | 21.08 | 51.81 | +145.8% | 47.44 → 19.30 |
| Runtime | 1 | 248.08 | 297.94 | +20.1% | 4.031 → 3.356 |
| Runtime | 32 | 242.63 | 272.64 | +12.4% | 4.121 → 3.668 |
| Runtime | 256 | 251.03 | 272.89 | +8.7% | 3.984 → 3.664 |

The candidate removes repeated subscription allocations but adds source-storage
locking and checkout/restore work. This fixture reports higher CPU cost for
every measured batch size. It does not establish the impact for sources whose
subscribe/cancellation/barrier work includes I/O or producer threads. The 256
connection runtime baseline varied from 241.80 to 260.27 ns per connection
between its two trials, illustrating the limits of this local comparison.

Raw Criterion estimates, confidence intervals, samples and outlier fences are
preserved in `timing-{baseline,candidate}-{1,2}.jsonl`, with matching stdout logs.
[timing-summary.json](timing-summary.json) records each trial and the aggregation.
[manifest.json](manifest.json) records source and executable SHA-256 hashes,
toolchain, features, commands and timing order. These are native backend results
from this machine, without native Windows or live Excel qualification.

[baseline-harness.patch](baseline-harness.patch) preserves the exact five-file
benchmark harness used with the old boxed-receiver API. It applies to the
baseline commit above. Applying it to clean copies of the three original files
and the two absent benchmark files was checked; all five resulting SHA-256
hashes match `baseline_sources_sha256` in the manifest. The manifest also hashes
the patch itself.

To restore and measure that baseline, run from the repository root, choosing an
unused worktree path:

```sh
report_dir="$(pwd)/benchmarks/rtd-source-slots"
baseline_dir=/tmp/xlfn-rtd-source-slots-baseline
git worktree add --detach "$baseline_dir" 34f32722ab49802f28617615a47d1b61948b97f7
git -C "$baseline_dir" apply "$report_dir/baseline-harness.patch"
cd "$baseline_dir"
XLFN_BENCH_REPORT_ONLY=1 cargo bench -p xlfn --features bench-internals,rtd --bench rtd_connection_allocations --locked
XLFN_BENCH_MEASUREMENT_MS=2000 cargo bench -p xlfn --features bench-internals,rtd --bench rtd_connection --locked -- --noplot --warm-up-time 1 --sample-size 50 --save-baseline source-slots-baseline-1
```

Reproduce a timing trial with:

```sh
XLFN_BENCH_MEASUREMENT_MS=2000 cargo bench -p xlfn --features bench-internals,rtd --bench rtd_connection --locked -- --noplot --warm-up-time 1 --sample-size 50
```

## Validation and API change

`RtdSubscription` now requires `Sized` and consumes `self` by value in
`disconnect_and_wait`. This intentionally breaks the previous boxed receiver
API. Registered source adapters preserve concrete subscription values in typed
slots and issue affine generation-checked ownership tickets. High-water slot
capacity is reclaimed at source teardown, as the zero owner-lifecycle allocation
delta verifies.

Validation for the integrated working tree:

- Workspace all-feature nextest: 1,449 passed, 9 skipped.
- Compile fixtures: 53 passed; feature powerset: 24 combinations passed.
- Clippy: all workspace targets; documentation and doctests passed.
- Miri under both Stacked Borrows and Tree Borrows: 10 cache,
  9 source-storage and 5 ticket/lifecycle cases per model, 48 passes total.
- Source-slot allocation gate and existing cache-registry, RTD preparation and
  RTD channel allocation gates passed.
- External end-to-end fixture and Excel-comparison native compile checks passed.
- Windows MSVC x86/x64 library cross-checks passed with `blake3/pure`; native
  Windows and live Excel execution were not performed.

Validation logs are under `/private/tmp/xlfn-source-slots-20261027`; these local
logs are separate from the repository allocation and timing evidence here.
