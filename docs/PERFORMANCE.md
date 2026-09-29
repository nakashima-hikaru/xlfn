# Value boundary and RTD optimization review — 2026-09-29

## Decisions

| Issue | Decision | Remaining boundary |
| --- | --- | --- |
| Semantic UTF-16 identity decoded every scalar twice | Use the shared unit-wise UTF-8 length plan, narrow proven ASCII directly, and strictly decode other text once. Invalid UTF-16 leaves a sticky encoder error. | Identity bytes and argument framing remain unchanged. |
| Each large argument allocates its own hash workspace | Reuse one owned workspace between arguments in `InputFingerprintBuilder`; reset hash state and overwrite the active prefix on promotion. | One large argument still allocates once; independent/nested calls have independent buffers. No thread-local owned storage or new DLL unload obligation. |
| Channel publication repeats admission and shard locking per value | Publish the existing bounded FIFO batch through one admission guard. Reuse the shard lock only within an already-notified epoch; unlock and notify before processing the next value when required. | Keep the 32-value turn limit, per-value cancellation/open checks, generation validation, dedup, quotas, sequence numbers, and epoch rechecks. |
| String arrays grow several arena chunks | Reject first-string-based speculative reservation; retain demand-driven arena growth. Add sparse-array allocation coverage and mixed-array lifetime coverage. | The first string cannot predict later cell types or sizes. A reliable sizing pass would need a separate design and workload qualification. |

The retained hash workspace lives only until its fingerprint builder finishes or drops, including failure paths. Constructor/finalizer inlining lets the compiler remove some unnecessary single-argument bookkeeping.

No production public API or fingerprint format changes. Benchmark support adds fixtures behind `bench-internals`.

## Measurement method

Baseline: `4092109cd6d2222fd83d1c4f81373a14da2830d8`. Both revisions use the added UTF-16 and multiple-argument benchmark fixtures. Release builds use Rust 1.98.1 / LLVM 22.1.8 on local macOS arm64, with `bench-internals,async,rtd` and the locked dependencies. Baseline and candidate are built in separate target directories and their immutable executable hashes are checked before measurement.

Initial exploration used 200 ms warmup, 1 s measurement and 20 samples. Final comparisons run baseline then candidate, followed by candidate then baseline, with 300 ms warmup, a 1.5 s measurement target and 30 samples (Criterion may extend collection to obtain all samples). No builds or tests run concurrently with timing. Times include fingerprint completion/destruction or the actual RTD pipeline cycle. Setup and input construction are outside timing. Allocation counts come from a separate system-allocator probe after 100 warm calls, over 10,000 measured calls; requested bytes are allocation traffic, not retained memory or RSS.

The RTD pipeline covers 1/4 producers and channel capacities 64/1024, publishing 10,000 values **per producer**. It exercises the production sender, shared publisher, erased sink and refresh path. These fixtures have no attached Excel notifier; they measure admission batching and ordinary publication, not the full benefit of holding the shard lock in an already-notified epoch. Notification/refresh/cancellation behavior is tested separately.

## Results

Values below are the arithmetic means of the two Criterion slope point estimates, not pooled confidence intervals. Pair ranges are the observed changes in the two order-reversed comparisons. Negative percentages mean less elapsed time. [All estimates and confidence bounds](performance/2026-09-29-paired.json) and [environment/artifact hashes](performance/2026-09-29-environment.json) are retained.

### input_identity

| Workload | Before | After | Change | Two-pair range |
| --- | ---: | ---: | ---: | ---: |
| utf16/ascii_short | 296.54 ns | 295.30 ns | -0.4% | -0.4% to -0.4% |
| utf16/ascii_1k | 3.48 µs | 2.30 µs | -33.9% | -34.0% to -33.9% |
| utf16/unicode_short | 302.54 ns | 302.25 ns | -0.1% | -0.4% to +0.2% |
| utf16/unicode_1k | 2.80 µs | 2.62 µs | -6.4% | -6.5% to -6.3% |
| utf16/unicode_limit | 209.79 µs | 193.78 µs | -7.6% | -7.9% to -7.3% |
| utf16/unicode_sparse | 3.48 µs | 3.33 µs | -4.3% | -5.4% to -3.2% |
| f64 | 276.00 ns | 287.22 ns | +4.1% | +3.6% to +4.5% |
| string_short | 279.95 ns | 291.94 ns | +4.3% | +3.8% to +4.8% |
| matrix_f64_16 | 713.91 ns | 714.37 ns | +0.1% | -0.4% to +0.6% |
| eight_matrix_f64_16 | 4.60 µs | 3.68 µs | -20.0% | -21.7% to -18.3% |
| matrix_f64_256 | 4.11 µs | 4.21 µs | +2.3% | -0.1% to +4.7% |
| eight_matrix_f64_256 | 31.72 µs | 31.44 µs | -0.9% | -3.4% to +1.6% |
| matrix_f64_4096 | 36.65 µs | 37.63 µs | +2.7% | -0.4% to +5.7% |
| eight_matrix_f64_4096 | 291.43 µs | 297.31 µs | +2.0% | -0.4% to +4.4% |
| matrix_f64_100k | 884.50 µs | 898.85 µs | +1.6% | -0.6% to +3.9% |

### rtd_publish

| Workload | Before | After | Change | Two-pair range |
| --- | ---: | ---: | ---: | ---: |
| number/changing | 356.20 µs | 331.21 µs | -7.0% | -7.4% to -6.6% |
| number/same_value | 249.15 µs | 266.38 µs | +6.9% | +6.5% to +7.4% |
| string/changing | 527.80 µs | 519.84 µs | -1.5% | -2.5% to -0.5% |
| string/same_value | 462.77 µs | 467.73 µs | +1.1% | +0.8% to +1.3% |
| string_8k/changing | 3.968 ms | 3.923 ms | -1.1% | -1.6% to -0.6% |
| string_8k/same_value | 3.879 ms | 3.862 ms | -0.4% | -0.8% to -0.1% |
| channel_pipeline/p1_c64 | 589.91 µs | 473.94 µs | -19.7% | -29.4% to -9.5% |
| channel_pipeline/p1_c1024 | 761.20 µs | 474.05 µs | -37.7% | -42.4% to -33.1% |
| channel_pipeline/p4_c64 | 4.159 ms | 4.556 ms | +9.5% | +5.3% to +13.0% |
| channel_pipeline/p4_c1024 | 4.385 ms | 3.820 ms | -12.9% | -16.2% to -8.1% |

The UTF-16 and workspace changes are retained for long-text identity and multi-argument allocation traffic. Single scalar/short-string identity is about 11–12 ns (+4%) slower; large numeric matrices do not demonstrate a timing improvement. These costs are included in the adoption decision.

RTD batching is retained for the 20–38% single-producer improvement and the 13% improvement with four producers/capacity 1024. Four producers/capacity 64 regress by 9.5% (both comparisons regress, despite scheduling variation), and direct repeated-number publication regresses by 6.9%, about 1.7 ns per update. This is an explicit throughput tradeoff, not a universal speedup. No capacity-specific fallback or benchmark-tuned batch-size rule is added; the existing 32-value turn bound is preserved.

The allocation probe confirms that eight 1,000-cell numeric arguments fall from **8 allocations / 48,192 requested bytes to 1 allocation / 6,024 bytes per call**. A single large argument still uses one allocation. Numeric and handle ingress remain allocation-free. After rejecting speculative reservation, string-array and sparse-array allocation counts/bytes match the baseline. [Allocation records](performance/2026-09-29-allocations.json).


[Initial exploration data](performance/2026-09-29-exploration.json) includes the rejected candidate. The rejected array reservation reduced allocations for 1,000 repeated `Ready` cells from 6 to 2, but raised allocation traffic for one string plus 999 numeric cells from 32,496 to 44,272 bytes (+36%). The initial 16,384-short-string timing rose from 203.40 to 233.81 µs (+15%). This is why the reservation is absent from production.

## Validation and remaining limits

[Validation counts](performance/2026-09-29-validation.json) record the local checks. Tests cover owned/UTF-16 identity equivalence, malformed surrogate rejection, inline/hash thresholds, buffer reuse across lengths, nested encoders, sparse/mixed arrays, deduplication, accepted-prefix delivery on sequence overflow, older refresh completion, refresh during notification, and cancellation of a drained channel batch.

Process-isolated nextest passes 986 tests (11 skipped). The workspace also passes serialized libtest, Clippy for all targets/features, 28 feature combinations, rustdoc with warnings denied, format/diff checks, and the panic-boundary audit (50 reviewed references).

Targeted Miri passes with `nightly-2026-08-22` under both Stacked and Tree Borrows. Strict provenance is enabled for identity workspace reuse, malformed UTF-16, mixed-array lifetime, and batched publication tests. The existing concurrent channel cancellation test uses the repository's normal Miri provenance mode: strict provenance stops in `parking_lot_core 0.9.12`'s integer-to-pointer queue reconstruction. This dependency limitation is not a passing strict-provenance result for that channel test.

Local performance is not Windows/Excel performance evidence. No live Excel/COM run, native Windows timing, release qualification, version change, or publication is part of this change. Short benchmark runs characterize these workloads only; concurrency and tail latency still need deployment-host qualification.

## Reproduction

Build the baseline checkout and candidate with identical benchmark fixtures and separate `CARGO_TARGET_DIR` values. The baseline allocation probe must omit the new `multiple == single` assertion (it deliberately fails before workspace reuse). Build and copy the executables before running either revision. For each immutable executable:

```sh
XLFN_BENCH_MEASUREMENT_MS=1500 path/to/input_identity --bench --noplot \
  --warm-up-time 0.3 --sample-size 30 --save-baseline LABEL
XLFN_BENCH_MEASUREMENT_MS=1500 path/to/rtd_publish --bench --noplot \
  --warm-up-time 0.3 --sample-size 30 --save-baseline LABEL
path/to/value_boundary_allocations
path/to/two_phase_ingress
```

Use `cargo bench -p xlfn --locked --features bench-internals,async,rtd --no-run --message-format=json` with the relevant `--bench` names to locate executables. Do not compare a baseline relinked from candidate artifacts in a shared target directory.
