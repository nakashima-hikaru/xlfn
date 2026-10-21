# Rust 1.99.1 native input ABI remeasurement — 2026-10-21

Rust 1.99.1 (`58cf8f9c3`, LLVM 23.1.1) replaces Rust 1.99.0 as the workspace toolchain and declared MSRV. The README, guide requirements, standalone consumer manifests, and current workspace test fixtures follow the same version. Historical measurements and independently selected Verus/Miri compilers are retained. No dependencies or production Rust sources changed.

## Method and measurement quality

The current Git HEAD is freshly built with both compilers from **439 identical Rust source files** and the same Cargo.lock. Separate source mappings and Cargo target directories prevent stale compiler artifacts. The rebuilt 1.99.0 executable matches the SHA-256 of the earlier ABI result bundle, and all of that bundle's recorded source hashes match the fresh baseline. Current compiler comparisons nevertheless use fresh interleaved measurements. Frozen binaries, full compiler details, source hashes, build logs and commands are recorded in provenance.json; upgrade.patch contains the version/configuration edits.

The first pass covers 24 paired input/mode cases (48 paths per compiler), with 30 Criterion samples, 150 ms warmup and 1 second measurement per case. Four processes per compiler/ABI cell give 16 process runs. Some samples have large within-run variance and run-to-run drift. They are retained without deleting slow observations, and are not sufficient on their own to establish compiler improvements. A process-name/CPU snapshot showed no competing compiler build at the time observed; the source of the variance is unresolved.

The confirmation repeats four representative cases with **1 second warmup and 3 seconds measurement**, again using 30 samples and four runs per compiler/ABI cell (16 more processes). Both passes interleave compiler versions; for each compiler, ABI order is Q/native/native/Q followed by native/Q/Q/native. Exact orders and raw estimates are preserved. No builds or tests ran concurrently with timing.

All table values are medians of four per-run mean estimates, in ns per call. Negative percentages mean lower latency. Two independent ordering-block percentages are retained in comparison.json. A cell is marked noisy if any run has standard deviation/mean above 10% or if the span of its four run means exceeds 10%; this is a measurement-quality screen, not a statistical significance test. Small percentage changes do not establish a speed improvement.

## Longer confirmation: Q versus native on 1.99.1

| Input / mode | Q ns | Native ns | Native change | First / second block | Noisy 1.99.1 cells |
| --- | ---: | ---: | ---: | ---: | --- |
| bool/prepare_identity | 243.8 | 245.6 | +0.8% | +0.1% / +1.0% | True |
| f64_1/plain | 24.4 | 24.8 | +1.7% | +3.6% / +1.5% | False |
| f64_8/plain | 37.3 | 29.0 | -22.1% | -22.8% / -16.2% | True |
| unicode_short/plain | 74.3 | 58.5 | -21.2% | -22.4% / -20.7% | False |

Interpretation: the compiler update does not show a consistent general speed improvement. On 1.99.1, short Unicode native admission is 21.2% below Q, and one-double native admission is 1.7% above Q. Eight-double native admission is 22.1% below Q in the median, but its 1.99.1 native cell remains flagged for variance (block estimates -22.8% and -16.2%). Boolean identity is also flagged; its small ABI/compiler differences remain inconclusive. No slow sample was discarded.

## Longer confirmation: compiler 1.99.0 to 1.99.1

| Input / mode / ABI | 1.99.0 ns | 1.99.1 ns | Change | First / second block | Noisy cell |
| --- | ---: | ---: | ---: | ---: | --- |
| bool/prepare_identity/q | 233.8 | 243.8 | +4.3% | +4.8% / +3.6% | False |
| bool/prepare_identity/native | 240.7 | 245.6 | +2.0% | +0.7% / +2.3% | True |
| f64_1/plain/q | 24.2 | 24.4 | +0.7% | +0.8% / +0.7% | False |
| f64_1/plain/native | 24.6 | 24.8 | +0.9% | +2.6% / +0.7% | False |
| f64_8/plain/q | 37.1 | 37.3 | +0.4% | +0.5% / -0.4% | False |
| f64_8/plain/native | 28.3 | 29.0 | +2.7% | +1.7% / +10.4% | True |
| unicode_short/plain/q | 74.1 | 74.3 | +0.2% | +0.7% / -0.1% | False |
| unicode_short/plain/native | 58.9 | 58.5 | -0.7% | -2.5% / -0.1% | False |

## Retained first pass: exploratory results

These are the original full-matrix medians. Consult the noise flags and original run estimates before interpreting any difference; confirmation results above take precedence for their four cases. The unconfirmed rows remain exploratory.

| Input / mode | 1.99.1 Q ns | 1.99.1 native ns | Native change | Noisy cells in either compiler |
| --- | ---: | ---: | ---: | --- |
| bool/materialize_identity | 253.3 | 239.9 | -5.3% | True |
| bool/plain | 26.6 | 24.9 | -6.7% | True |
| bool/prepare_identity | 290.6 | 241.4 | -16.9% | True |
| f64_1/materialize_identity | 239.2 | 250.2 | +4.6% | True |
| f64_1/plain | 24.4 | 25.8 | +6.0% | True |
| f64_1/prepare_identity | 240.0 | 251.2 | +4.7% | True |
| f64_2/materialize_identity | 384.7 | 387.6 | +0.8% | True |
| f64_2/plain | 25.0 | 26.0 | +4.0% | True |
| f64_2/prepare_identity | 389.0 | 387.1 | -0.5% | True |
| f64_4/materialize_identity | 621.5 | 604.1 | -2.8% | True |
| f64_4/plain | 28.2 | 26.2 | -7.0% | True |
| f64_4/prepare_identity | 622.6 | 602.7 | -3.2% | True |
| f64_8/materialize_identity | 1060.3 | 1043.7 | -1.6% | True |
| f64_8/plain | 38.1 | 28.2 | -26.1% | True |
| f64_8/prepare_identity | 1066.0 | 1034.1 | -3.0% | True |
| i32/materialize_identity | 243.7 | 238.9 | -2.0% | True |
| i32/plain | 25.7 | 24.8 | -3.7% | True |
| i32/prepare_identity | 250.2 | 239.9 | -4.1% | True |
| unicode_long/materialize_identity | 24614.4 | 25074.9 | +1.9% | True |
| unicode_long/plain | 4105.8 | 4164.6 | +1.4% | True |
| unicode_long/prepare_identity | 20543.9 | 21189.0 | +3.1% | True |
| unicode_short/materialize_identity | 386.3 | 389.6 | +0.9% | True |
| unicode_short/plain | 73.6 | 59.1 | -19.6% | True |
| unicode_short/prepare_identity | 340.5 | 342.4 | +0.6% | True |

`plain` includes ordinary conversion/materialization. `prepare_identity` validates and fingerprints strings before discarding the prepared value without creating an owned String. `materialize_identity` also creates the String; scalar materialization is trivial. The short Unicode text is `日本語💡`; the long input repeats it 800 times (4,000 UTF-16 units). Fixture construction is excluded; ordinary call admission and scratch cleanup are included. Handle lookup/publication, generated entrypoint dispatch, Excel preprocessing, COM, and actual Excel recalculation are excluded.

## Validation and scope

Rust 1.99.1 workspace nextest: 1,310 passed. Strict all-target/all-feature Clippy, strict rustdoc, formatting, release metadata, guide validation (17 chapters), Verus installer tests (8), build-time tool tests (9), and x86_64/i686 MSVC library cross-checks passed. All three standalone consumers and the default/instrumented Excel fixtures also pass compilation; the standalone ABI probe has 1 local test passed and 1 SDK test ignored. Exact commands are in validation.json and standalone-validation.json.

Native Windows SDK ABI execution and live Excel N01–N06 are unexecuted. Miri remains pinned to its independent nightly and was not rerun for this compiler-only migration. The 288 feature powerset and full bench-ci/bench-scaling suites were not rerun. These local measurements do not qualify Windows/Excel speed, coercion semantics, or universal compiler improvements.

Reproduce the benchmark cases with `rtk just bench-native-abi`. See [the Excel comparison guide](../../README.md) for Windows build and qualification commands. The previous Rust 1.99.0 bundle is preserved separately.
