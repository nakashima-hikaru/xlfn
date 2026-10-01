# Rust 1.99 adoption measurements

This comparison measures the Rust 1.99 ownership/lint adoption against the
committed Rust 1.98.1 baseline. Both source and compiler change, so the timing
and size differences combine source changes with compiler effects. They do
not isolate LLVM 23.

The runner freezes both source trees, uses separate Cargo target directories,
records `rustc -vV`, source and executable SHA-256 values, and runs Criterion
executables in baseline/candidate/candidate/baseline order. It verifies the
Cargo log's executable against recorded build provenance before freezing it.
Rust source/build paths are remapped to the same virtual paths to remove
embedded source-path differences. Existing caller rustflags
are preserved, and effective encoded rustflags are saved for every build.
Compilation is excluded from timing. The fixtures cover synchronous UDF
admission/return, warm cache hits, cold misses, async spawn/rescheduling, RTD
publish/refresh, and extra function registration counts 0/10/100/1000/5000.

Criterion timings describe each helper's complete batch on the measurement
host. They do not measure live Excel UDF latency. Add-in size includes the
complete unstripped fixture library; its least-squares slope is bytes per
extra registered identity function over the recorded count range.

## Recorded macOS comparison

The [paired record](rust-1.99-paired.json) contains 40 measurements: ten cases,
two runs of each frozen executable version. It used Apple M1, macOS 27.0,
SDK 15.2, release mode and the system allocator. Rust 1.98.1 reports LLVM
22.1.8; Rust 1.99.0 reports LLVM 23.1.1. The baseline commit is
`b9596f8e221ef3804bb802c0795d7e7e8660f8df`; the candidate is the frozen worktree
containing the Rust 1.99 adoption changes. Later formatting, documentation
and test-fixture edits do not change these benchmark paths.

Values below are the median of each version's two per-run Criterion medians.
Negative change means less elapsed time. These are batch times, including
the existing harness coordination costs.

| Case | Batch | 1.98.1, µs | 1.99.0, µs | Change |
| --- | ---: | ---: | ---: | ---: |
| Sync admission | 1,000 calls | 26.719 | 26.566 | -0.57% |
| Sync scalar return, no subscriber | 1,000 calls | 54.133 | 52.034 | -3.88% |
| Warm cache hit | 1,000 hits | 29.945 | 31.571 | +5.43% |
| Cold cache, distinct keys | 256 requests, 1 worker | 52.112 | 51.216 | -1.72% |
| Cold cache, same keys | 1,024 requests, 4 workers | 145.669 | 142.925 | -1.88% |
| Async spawn | 128 attempts | 103.057 | 104.421 | +1.32% |
| Async reschedule | 1,024 attempts, 4 yields each | 1,022.103 | 1,012.291 | -0.96% |
| RTD number publish, changing | 10,000 publications | 358.067 | 338.314 | -5.52% |
| RTD number publish, same value | 10,000 publications | 276.344 | 269.176 | -2.59% |
| RTD number refresh, dense | 4,096 updated topics | 335.939 | 336.536 | +0.18% |

The warm-hit regression repeated in a separate short ABBA run using the same
frozen binaries: 29.652 → 31.173 µs per 1,000-hit batch, **+5.13%**, or about
1.52 ns more per hit. The [repeat record](rust-1.99-cache-hit-repeat.json)
retains its four measurements and executable hashes. The adoption therefore
has a measured warm-hit cost on this host; the results do not establish a
general speedup. The short run and helper workloads do not explain the cause
or establish statistical significance for every small change in the table.

| Extra registered functions | 1.98.1 library, bytes | 1.99.0 library, bytes | Change |
| ---: | ---: | ---: | ---: |
| 0 | 1,708,144 | 1,724,240 | +0.942% |
| 10 | 1,770,848 | 1,789,920 | +1.077% |
| 100 | 2,445,664 | 2,447,456 | +0.073% |
| 1,000 | 9,030,224 | 9,055,392 | +0.279% |
| 5,000 | 38,336,128 | 38,377,040 | +0.107% |

The least-squares slope over these five counts is **7,326.03 → 7,331.65 bytes
per extra function (+0.077%)**. This records a small fixture-size increase.
The unstripped Mach-O libraries retain their physical output name in
`LC_ID_DYLIB`, outside Rust's source-path remapping: its padded command is
176 bytes for baseline and 184 for candidate at every count. That fixed
8-byte metadata difference is included in file sizes and has no effect on
the registration slope. Windows PE library sizes remain unmeasured.

## Reproduction

Install both stable toolchains and, for Windows, both MSVC targets. Use a
native Windows host to execute MSVC benchmark binaries. Preserve the baseline
commit in the checkout (`fetch-depth: 0` in CI).

```sh
rtk proxy rustup toolchain install 1.98.1 1.99.0 --profile minimal
rtk proxy rustup target add --toolchain 1.98.1 i686-pc-windows-msvc x86_64-pc-windows-msvc
rtk proxy rustup target add --toolchain 1.99.0 i686-pc-windows-msvc x86_64-pc-windows-msvc
rtk proxy python3 -B benchmarks/performance/toolchain_compare.py \
  --baseline-ref b9596f8e221ef3804bb802c0795d7e7e8660f8df \
  --target x86_64-pc-windows-msvc \
  --work-dir target/toolchain-comparison/x86_64-pc-windows-msvc
```

Repeat with `i686-pc-windows-msvc` and a fresh output directory. Omit `--target`
for a host-native run. On the recorded macOS installation, set
`SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX15.2.sdk` for the build.

The default snapshots the current candidate worktree; pass `--candidate-ref`
to compare a committed candidate. For a compiler-only control, use the same
Rust-1.98-compatible source revision on both sides. Candidate sources that
use stable Rust 1.99 APIs cannot be compiled by Rust 1.98.1.

Use `--phase prepare` to finish all builds first, then `--phase measure` with
the same `--work-dir` during an otherwise idle session. The default duration
is one second per case with a 0.3-second warmup and 50 samples. The unchanged
`sync_boundary` harness fixes its group duration at ten seconds; the runner
applies and records ten seconds for that executable. JSON records distinguish
the requested default from each executable's effective duration.

`paired.json` includes timings and both complete build/size provenance
records. Build logs and frozen library/benchmark binaries remain in the output
directory. The existing Windows benchmark CI continues to run `just bench-ci`;
the separate manual toolchain comparison workflow runs this paired entry point.

## Evidence boundary

Native MSVC performance and size results, on both Windows targets, remain a
separate qualification step. macOS timings, cross-target checks and Miri do
not establish Windows runtime performance or live Excel behavior.

The [validation record](rust-1.99-validation.json) records local/cross-target
checks. The [Pin contract audit](../../tools/rust-1.99-pin-audit.md) documents
the async future projections and their Miri coverage.
