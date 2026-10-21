# ObjectCell pointer removal — 2026-10-21

Adopt removal of the redundant `ObjectCell::pointer`. Retain cached `TypeId`,
concrete type name, and `Option<Box<dyn Any + Send + Sync>>`. Successful type
checks project the pointer from the retained owner without an `Any::type_id`
or `downcast_ref` call. Ownership, counting, release, and panic containment
remain unchanged.

The actual 64-bit layout changes from 80 to 72 bytes. All four allocation
comparisons show exactly 8,000 fewer requested live bytes for 1,000 objects and
80,000 fewer for 10,000. However, this Mac's System allocator reports an 80-byte
usable allocation for both requests (10,000 samples per size). These results do
**not** establish physical allocation savings or lower RSS on this host.

Growth/churn allocation deltas return to zero after teardown. The first sparse
case retains a 288-byte post-drop delta in both variants in every process; the
other sparse case returns to zero. This common first-case delta is preserved in
the evidence and is not attributed to the pointer change.

## Decision and tradeoffs

Rotating 65,536-object lookup has paired median changes of -0.86%, -6.88%,
-3.80%, and -5.37% at 1/4/16/32 workers. The 1/4/32-worker improvements have the
same sign in all four pairs; 16 workers has one slower pair. The 32-worker case
has substantial scheduling variability, including a -39% pair. These are local
workload results, not a stable production speed guarantee.

Fixed distinct-token lookup at four workers is 1.08% slower (all four pairs).
Same-token lookup at 32 workers is 1.17% slower by paired median. Creation of
10,000 objects is 1.23% slower by paired median, with mixed signs across pairs;
dropping 10,000 is 2.99% faster. The smaller layout and rotating-working-set
results favor adoption with these small tradeoffs. There is no universal
lookup, creation, or destruction speedup claim.

## Method and provenance

Baseline: `319884f8691df1b72a9d62f13b3571a87da1127d`, with identical shared benchmark additions.
Rust 1.99.0, LLVM 23.1.1, aarch64 macOS 27.0.1. Sources and Cargo targets are
separate. Four frozen executables per variant have SHA-256 verified before every
invocation. Features are `bench-internals async`, without refinement tracing.

ABBA followed by BAAB gives four processes per variant, 22 cases per process,
176 Criterion results. Focused measurement is 1,000 ms with 0.2 s warmup. CLI
sample size is 30; groups may override it. Population/thread creation stay outside
lookup timing, while dispatch/completion remain included. Rotating cursors persist
across batches. Destruction fixtures are populated outside destruction timing.
No other validation jobs from this task overlap paired timing.

The JSON retains commands, toolchain, source hashes, candidate diff, executable
hashes, scripts, all raw timing samples/estimates, and all allocation runs.

Times are the median of four process-level Criterion mean estimates, in us/batch.
The paired change is the median of four adjacent-pair ratios; it need not equal
the ratio of the two time columns. Negative means faster. Pair ranges show
variation and are not confidence intervals or significance tests.

| Case | Baseline us/batch | Candidate us/batch | Paired change | Pair range |
| --- | ---: | ---: | ---: | ---: |
| `handle_lookup/distinct_tokens/1` | 25.910 | 25.808 | -0.56% | -0.82% to +0.36% |
| `handle_lookup/distinct_tokens/16` | 136.218 | 137.602 | +1.06% | -1.08% to +2.69% |
| `handle_lookup/distinct_tokens/32` | 235.873 | 221.918 | -6.77% | -20.16% to +1.64% |
| `handle_lookup/distinct_tokens/4` | 54.633 | 54.906 | +1.08% | +0.08% to +1.91% |
| `handle_lookup/rotating_65536_tokens/1` | 296.755 | 294.624 | -0.86% | -3.44% to -0.32% |
| `handle_lookup/rotating_65536_tokens/16` | 1154.304 | 1119.755 | -3.80% | -5.84% to +1.63% |
| `handle_lookup/rotating_65536_tokens/32` | 2463.947 | 2331.580 | -5.37% | -38.97% to -4.03% |
| `handle_lookup/rotating_65536_tokens/4` | 398.776 | 372.448 | -6.88% | -10.66% to -3.06% |
| `handle_lookup/warm_same_token/1` | 26.112 | 26.002 | -1.13% | -14.69% to +0.57% |
| `handle_lookup/warm_same_token/16` | 141.175 | 137.448 | -1.84% | -5.76% to +0.98% |
| `handle_lookup/warm_same_token/32` | 225.337 | 226.692 | +1.17% | -2.04% to +5.23% |
| `handle_lookup/warm_same_token/4` | 55.285 | 54.680 | -1.40% | -15.32% to +0.61% |
| `handle_prepare/cold_grow/1000` | 838.717 | 837.282 | -0.17% | -14.05% to +0.56% |
| `handle_prepare/cold_grow/10000` | 8658.394 | 8692.993 | +1.23% | -7.45% to +4.99% |
| `handle_prepare/cold_miss_batch_100` | 80.249 | 80.380 | +0.16% | -2.74% to +0.38% |
| `handle_prepare/drop/1000` | 765.406 | 770.817 | +0.84% | -0.80% to +1.09% |
| `handle_prepare/drop/10000` | 8358.106 | 8105.859 | -2.99% | -5.67% to +0.00% |
| `handle_prepare/republish/10_000` | 16747.747 | 16631.855 | -0.77% | -9.86% to +0.93% |
| `object_lease/distinct_objects/4` | 87.861 | 88.030 | +0.20% | -0.90% to +4.37% |
| `object_lease/final_pin_release` | 0.034 | 0.034 | -1.33% | -4.56% to +2.22% |
| `object_lease/pin_acquire_release_serial` | 0.019 | 0.019 | -0.94% | -3.37% to +1.04% |
| `object_lease/same_object/4` | 161.632 | 159.459 | -1.28% | -2.49% to -0.95% |

## Validation and limits

- Workspace nextest/all features: 1,298 passed, 11 skipped.
- Handles-only nextest: 545 passed, 8 skipped.
- Workspace Clippy/all targets/all features and strict rustdoc: passed.
- Panic boundaries: passed (5 checker tests, 49 reviewed direct references).
- Miri object tests: 7 passed under each of Stacked Borrows and Tree Borrows.
- Both new projection tests additionally pass strict provenance under both models:
  overlapping shared borrows, retained pin after binding retirement, wrong type,
  exactly-once destruction, and an over-aligned zero-sized payload.
- Full-module strict provenance encounters existing `parking_lot_core` 0.9.12
  integer-to-pointer queue tagging. Ordinary Miri checks pass without disabling
  borrow tracking, leak detection, or validity checks.
- `just bench-ci`: passed, 124 Criterion cases, measurement shortened to 200 ms.
  It finished before paired timing and serves as an execution gate; comparative
  claims use the focused paired runs above.
- i686 Windows MSVC all-feature cross-check: passed.
- x86_64 Windows MSVC cross-check: blocked by blake3 C/ASM compilation requiring
  the Windows assembly toolchain on this Mac.
- Native Windows and live Excel: not run.
- Final formatting, diff checks, and frozen executable/source hashes: passed.

See [complete JSON evidence](2026-10-21-object-cell-pointer.json). Frozen binaries
and raw logs remain under `/tmp/xlfn-object-pointer-20261021/` for inspection.
