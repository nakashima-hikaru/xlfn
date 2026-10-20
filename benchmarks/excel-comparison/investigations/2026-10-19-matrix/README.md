# Matrix performance investigation — 2026-10-19

The supplied live Excel results show a real observed difference, but do not
identify its cause or establish a statistically significant framework-wide
regression. Two avoidable costs in xlfn were measured and removed. The original
millisecond-scale difference against Excel-DNA remains unqualified until the
updated XLL and runner are measured on Windows.

## Live Excel evidence

[Selected observations](excel-observations.json) retain the supplied artifact
hashes, commit, host, and p50 values. These are a transcription of selected
metrics, not the complete original results.jsonl. All selected pairs use one
calculation thread, five repetitions, Excel 16.0 build 20430, CI run 39176112243.

M02 xlfn/Excel-DNA p50 ratios are 2.21, 2.46, and 1.36 for 100, 10,000, and
100,000 cells. M04 ratios are 1.90, 1.53, and 1.65 for tall, square, and wide
10,000-cell inputs. M01/100x100 and M04/100x100 execute exactly the same SUM
formula and values. xlfn p50 changes from 2.555 to 2.518 ms, while Excel-DNA
changes from 2.654 to 1.646 ms in separate sessions. That alone demonstrates
substantial between-session variation in this run. It does not prove that all
of the implementation difference is measurement noise.

Each matrix case recalculates only one formula via Application.Calculate, then
queries CalculationState through COM. Thus ns_per_element includes dispatch,
Excel calculation/coercion/spill work, and completion queries. It is not the
cost of converting one element inside the add-in. Five within-session samples
and interpolated p95/p99 values do not quantify between-session uncertainty.

## Changes and contracts

* M02: expose the small push_f64, capacity/write helpers, and finite-output
  validator to cross-crate inlining. The downstream producer previously called
  a non-inlined public method for each element. This removed symbol is visible
  in the frozen macOS binaries. No allocation owner, return ABI, error ordering,
  or output policy changes. Validation still precedes capacity rejection.
* M04: reduce finite-value predicates in blocks of 64 before branching, allowing
  vectorization without changing the input buffer or numeric summation order.
  A rejected input reads at most the remainder of its current 64-element block;
  it does not scan the rest of a large invalid matrix. All declared elements
  are already required to be readable by the FP12 borrowing contract. Invalid
  input latency was not performance-qualified; the bounded extra work is a
  tradeoff. The error remains InputError::NonFinite before exposing the borrow.
* Runner: retain the old end-to-end total and save each measured matrix sample,
  split into calculate_call_s and completion_wait_s, plus completion_polls.
  Warmup is excluded. Calculate call time still includes COM and Excel; this
  instrumentation cannot isolate UDF execution time or assign the whole gap.
* Benchmarks: exercise push_f64 from the downstream benchmark crate (the old
  internal builder fixture hid the public call cost) and all M04 shapes. Both
  sets are included in the canonical bench-ci selection.

## Paired local measurement

Host: aarch64-apple-darwin, rustc 1.99.0 (b940084d7), LLVM 23.1.1,
SDKROOT MacOSX15.2.sdk; release profile, bench-internals enabled.
The baseline was a tracked working-tree snapshot at 1e569b7 plus the recorded
pre-existing diff. That diff was subsequently committed as ec715c3;
none of the matrix production functions changed. Benchmark-only additions were
installed identically in both snapshots before compilation.

Separate sources and CARGO_TARGET_DIRs were used. Executables were copied to
frozen paths and SHA-256 checked before every invocation. The order was
baseline/candidate/candidate/baseline, then candidate/baseline/baseline/candidate.
Each case used 100 ms warmup, 500 ms measurement, 30 samples, 1,000 bootstrap
resamples. No compilation or test load ran during the paired measurements.
Values below are the median of four per-process Criterion median estimates,
in nanoseconds; changes are candidate/baseline minus one. These short local
runs qualify the listed paths only, not Windows or whole Excel recalculation.

| Case | Baseline ns | Candidate ns | Change |
| --- | ---: | ---: | ---: |
| `argument_ingress/f64_plain` | 24.61 | 25.22 | +2.47% |
| `argument_ingress/numeric_matrix_ref_100000_with_identity` | 559662.94 | 548534.87 | -1.99% |
| `argument_ingress/numeric_matrix_ref_100k_plain` | 41342.28 | 13551.43 | -67.22% |
| `argument_ingress/numeric_matrix_shape_1000x10` | 3279.44 | 1354.67 | -58.69% |
| `argument_ingress/numeric_matrix_shape_100x100` | 3260.38 | 1338.71 | -58.94% |
| `argument_ingress/numeric_matrix_shape_10x10` | 52.05 | 33.49 | -35.66% |
| `argument_ingress/numeric_matrix_shape_10x1000` | 3281.10 | 1377.84 | -58.01% |
| `argument_ingress/numeric_matrix_shape_1x1` | 23.81 | 23.55 | -1.11% |
| `array_numeric_output/builder_build_return/100000` | 62943.64 | 63366.87 | +0.67% |
| `array_numeric_output/matrix_build_return/100000` | 78630.51 | 78440.22 | -0.24% |
| `array_numeric_output/matrix_convert_return/100000` | 60991.49 | 61478.28 | +0.80% |
| `array_numeric_output/numeric_slice_convert_return/100000` | 60378.11 | 60869.44 | +0.81% |
| `array_numeric_producer/push_f64/100` | 243.38 | 108.44 | -55.44% |
| `array_numeric_producer/push_f64/10000` | 23760.26 | 8194.84 | -65.51% |
| `array_numeric_producer/push_f64/100000` | 249024.59 | 80512.89 | -67.67% |

The public numeric producer includes allocation, seed+index computation,
checked cell writes, finish, and drop. It excludes Excel publication and spill.
Numeric ingress includes framework call setup and checked borrowing, but not
the subsequent SUM. Thus the roughly 58–59% M04 improvement is in input
validation, not the complete M04 workload. The 10,000-cell validation saving
is about 1.9 µs here, versus the observed Windows p50 gap of 0.87–1.25 ms.
The 100,000-cell producer saving is about 169 µs here, versus a Windows gap of
1.94 ms. Cross-host timings cannot be subtracted to estimate the Windows fix.

Controls: scalar ingress was +2.47% (+0.61 ns); the four existing 100k return
fixtures ranged from -0.24% to +0.81%. These changes are retained in the report
rather than claiming every path improved. Numeric identity was -1.99%.

[Summary with all process estimates](paired-summary.json),
[per-process observations](paired-raw.json),
[Criterion samples and estimates](criterion-observations.json),
[executable hashes](executables.json),
[baseline source hashes](sources-baseline.json),
[candidate source hashes](sources-candidate.json), and
[initial source/toolchain provenance](provenance.json) are stored beside this
report. Full Criterion distributions, frozen executables, exact source trees,
and paired.py are retained at
`/private/tmp/xlfn-matrix-20261019-3y90px7t`.
The source snapshots were hash-verified after measurement. Later regression
unit tests and formatting in the working tree do not change production logic.

Build commands, run separately in the baseline and candidate trees:

```sh
SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX15.2.sdk \
CARGO_TARGET_DIR=<separate-target-directory> \
cargo bench -p xlfn --features bench-internals --bench argument_ingress \
  --bench array_numeric_output --no-run --locked
```

Frozen binaries were invoked with `--bench`, the relevant case filter,
`--warm-up-time 0.1 --sample-size 30 --nresamples 1000 --noplot`,
`XLFN_BENCH_MEASUREMENT_MS=500`, and a separate CRITERION_HOME per run.

## Validation

* Workspace nextest, all features: 1,293 passed, 11 skipped.
* Feature powerset: all 288 library configurations passed cargo hack check.
* Canonical bench-ci: all 20 recipes passed, plus the five newly added gate
  cases run separately (the gate was already running when its selection changed).
  XLFN_BENCH_MEASUREMENT_MS=100 shortened this local execution. Other checks
  ran concurrently: these are execution/contract smoke results, not paired
  regression or CI performance claims.
* Excel runner unittest suite: 74 passed, one Windows-only dump test skipped.
* Strict workspace Clippy (all targets/features) and xlfn rustdoc passed.
* Numeric boundary Miri selection: four passed under default borrow checking,
  and four passed with Tree Borrows plus strict provenance; neither run reported
  a process-exit leak. Includes all positions of 1/63/64/65/127/128/129-cell
  payloads with NaN and both infinities, finite bit preservation, and builder
  recovery after rejection and full-builder error precedence.
* Windows x64 MSVC cross-check of the comparison fixture passed. This is a
  compilation check, not Windows execution or Excel validation.
* Panic-boundary audit, formatting, diff whitespace checks, and evidence/source
  hash consistency checks passed.

## Windows requalification

Use newly built CI artifacts and the runner from the same commit. Reusing the
old XLL only tests the new timing instrumentation, not the Rust optimizations.
In independent sessions, alternate xlfn/Excel-DNA and reverse the order. For
example, run the following four times with implementation order
xlfn, excel_dna, excel_dna, xlfn and distinct output filenames, then reverse that
order in a second block:

```powershell
py run.py --artifacts C:\path\to\new-artifacts --profile full `
  --id M01 --id M02 --id M04 --repeat 30 --threads 1 `
  --implementation xlfn --out matrix-A1.jsonl
```

Compare independent-session medians, retain all raw component timings and
artifact hashes, and include the duplicate M01/100x100 and M04/100x100 control.
The original live Windows difference is **not claimed resolved** by this
local change. No Windows execution, new CI artifact, or live Excel measurement
was performed here.
