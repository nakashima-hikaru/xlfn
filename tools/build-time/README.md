# Consumer build time

## Optional subsystem isolation (2026-10-07)

The core-only build no longer compiles the private handle, subscription, or
Excel RTD implementations. Handle permits and formula-caller decoding compile
only with `handles`. The small formula lifetime contract moved out of the
handle implementation, allowing RTD-only Windows builds to use the transport
without compiling handle state, registries, or tokens. RTD module state and
shutdown certificates live in a small neutral module; their issuance conditions,
phase checks, and atomic ordering are unchanged. Subscription and COM transport
code compile only with `rtd` or `handles`.

`constant_time_eq`, `getrandom`, `papaya`, and `blake3` are optional production
dependencies enabled by `handles`. The public `value::InputIdentityEncoder`
methods and the `ExcelInputIdentity` / `PrepareExcel` signatures remain available
in every feature selection. Without handles, the encoder is uninhabited and has
no constructor; formula dispatch cannot create an encoder or a substitute hash.
With handles, the original implementation and byte encoding are unchanged in
`input_identity/backend.rs`. BLAKE3 remains a dev dependency for the existing
core identity tests.

The baseline is commit `f0381bbb7be677703604afd505928f46696666ca`, which already
includes the opaque UDF body and serialization changes documented below. The
candidate is a frozen working-tree snapshot with this isolation change. These
results measure the combined module and dependency changes, not their separate
contributions. Both source snapshots and fixtures are hashed in the raw reports.

Measurements used Rust 1.99.0 on macOS arm64, offline dependencies, separate
target directories, compiler-cache wrappers disabled, and five pairs with
alternating order. No other build or test was run concurrently with timing.
Clean samples remove every compiled artifact and keep ordinary incremental
settings. The handles row uses the identical basic fixture with `handles`
enabled; it measures that compile graph rather than handle runtime performance.
The generated fixture contains 100 UDFs with 50 statements each. Its framework
rebuild mode cleans the consumer, `xlfn`, and `xlfn-macros` while retaining
third-party artifacts, disables incremental compilation, and excludes warmups.
It differs from the earlier consumer-only dependency-warm measurement.

| Workload | Baseline median | Candidate median | Change |
| --- | ---: | ---: | ---: |
| Basic add-in, clean dev | 7.0108 s | 5.9402 s | -15.3% |
| Basic add-in, clean release | 7.9019 s | 6.3394 s | -19.8% |
| RTD add-in, clean dev | 7.2982 s | 6.4678 s | -11.4% |
| Basic add-in with handles, clean dev | 7.2129 s | 7.2371 s | +0.3% |
| 100 UDFs, framework rebuild, check | 2.2033 s | 1.8437 s | -16.3% |
| 100 UDFs, framework rebuild, build | 3.1461 s | 2.7454 s | -12.7% |

Every candidate basic and RTD clean sample omitted all four handle dependencies.
Basic dev compiled 64 Cargo units instead of 83; release compiled 68 instead of
87. Handles compiled the same 83 units in both snapshots, and its +0.3% median
change does not establish a regression or speedup. One candidate handles sample
was 8.33 s; the others were 7.15–7.32 s, while baseline samples were 7.13–7.25 s.

For basic dev, the `xlfn` library unit median fell from 2.39 to 1.74 s; release
fell from 2.78 to 1.89 s. Unit durations overlap and must not be summed to infer
wall time. The `syn` feature set is identical in the two snapshots. Its duration
also changed, so dependency concurrency and host variation contribute to the
observed unit timings; this comparison does not attribute the savings to a
single crate or parser. The candidate basic critical path still includes
`syn` and `xlfn-macros`, so no crate split was added in this pass.

The macro clone/parser rewrite is deferred. Removing only xlfn-macros' AST
clones cannot disable `syn/clone-impls` in the basic graph: `thiserror-impl`
enables `syn/default`. Macro tests also enable cloning through
`trybuild -> serde_derive`. Removing `syn/full` remains a separate syntax change
that needs a narrow signature parser, `DeriveInput` struct parsing, opaque
default-expression tokens, and equivalent diagnostics before measurement.

Raw results: [basic dev](2026-10-07-feature-isolation-basic-dev.json),
[basic release](2026-10-07-feature-isolation-basic-release.json),
[RTD dev](2026-10-07-feature-isolation-rtd-dev.json),
[handles dev](2026-10-07-feature-isolation-handles-dev.json), and
[generated framework rebuild](2026-10-07-feature-isolation-generated-framework-warm.json).
Each sample stores complete Cargo unit data plus the 20 longest units.

Validation: core library tests passed (340, 7 ignored), RTD-only library tests
passed (512, 7 ignored), handles-only passed (489, 8 ignored), async-only passed
(445, 7 ignored), and all-feature library tests passed (873, 10 ignored), all
serially. The five timing-tool tests passed, including source hashes ignoring
nested Cargo build artifacts. Current library sources match the frozen
candidate hash. Core and all-feature compile-test suites passed, including custom
input implementations using every public encoder method. Every individual
feature and the all-feature library checked successfully; workspace all-target,
all-feature Clippy and core/all-feature rustdoc passed. The basic consumer and
RTD-only production library checked normally for Windows. Handles-only Rust
cross-checking passed with command-only
`blake3/pure`; its normal assembly backend still requires unavailable MSVC tools
on this host. Windows test compilation stopped in `alloca` because Windows C
headers were unavailable. Native Windows timing, linking, and Excel execution
are not covered by these measurements.

To reproduce this comparison, export the baseline commit named above and use the
commands under Reproduce with the new baseline directory. For the generated
framework rows, add `--workload generated --cache dependencies --rebuild framework
--incremental off --modes check build`.

## Clean builds (2026-10-07)

Normal add-in builds used to compile `serde_derive` for private verification
traces and two diagnostic record types. Trace serialization now compiles only
for tests or the `refinement` feature. Production diagnostic records implement
`Serialize` directly, preserving the existing JSON schema and escaping. The
public `serde` feature and the packaging tools still enable the derive macros.
No optimization, debug-information, or default-feature settings are reduced.

The clean benchmark builds the actual standalone example `cdylib`, removing
**all compiled artifacts, including every dependency**, before each sample.
Downloads and the filesystem cache remain warm; builds run offline. Separate
target directories and alternating baseline/candidate order are used for five
pairs, with compiler-cache wrappers disabled. Cargo incremental settings retain
their ordinary defaults. The host is macOS arm64 with Rust 1.99.0.

| Consumer | Profile | Baseline median | Candidate median | Reduction |
| --- | --- | ---: | ---: | ---: |
| Basic add-in | dev | 7.4295 s | 6.7699 s | 8.9% |
| Basic add-in | release | 8.6075 s | 7.8269 s | 9.1% |
| RTD add-in | dev | 8.4831 s | 7.7762 s | 8.3% |

Raw results: [basic dev](2026-10-07-clean-basic-dev.json),
[basic release](2026-10-07-clean-basic-release.json),
[RTD dev](2026-10-07-clean-rtd-dev.json). Every baseline sample
compiled `serde_derive`; every candidate sample omitted it. The RTD baseline
samples varied from 8.20 to 10.67 s, so its median is less stable than the basic
consumer measurements. The baseline is a
working-tree snapshot after the opaque UDF body change below, before the
serialization cleanup, based on commit
`c396e54be1ecb4dd8f3da5ac27c37daf35ae09f3`. Source and consumer hashes are in
each result file. These comparisons isolate the additional clean-build change.

Downstream projects that already enable `serde/derive` through another dependency
will share that dependency, so they should not expect the same savings. Windows
XLL linking and native Windows clean-build timing have not been measured.

Validation: the all-feature library suite passed with 867 tests (10 ignored)
when run serially, including JSON schema/escaping and notification retry tests.
Workspace all-target/all-feature Clippy passed, as did standalone consumer
checks with async/handles/cache/bench-internals and with serde/refinement.
The first parallel run had one async responder-count failure; that test passed
in isolation and in the serial suite. This is retained as a validation limit,
not evidence of an RTD/Excel fix.

## Dependency-warm UDF rebuilds (earlier measurement)

`excel_function` inspects a function's attributes and signature. Its body is
emitted unchanged, so parsing every body expression with `syn::ItemFn` duplicated
the Rust compiler's work. The macro now retains body tokens and their spans,
while still parsing outer/inner attributes and the complete signature.
Rust checks the body as usual. Public syntax, generated wrappers, dependency
features, optimization settings, and debug information are unchanged.

### Measurement (2026-10-07)

Baseline: `c396e54be1ecb4dd8f3da5ac27c37daf35ae09f3`.
The [raw results](2026-10-07-udf.json) contain toolchain, platform, source/fixture
digests, every sample, and Cargo frontend/codegen timings.

The fixture uses the basic add-in plus 100 thread-safe UDFs, each with 50
arithmetic statements. Both trees build the identical fixture with Rust 1.99.0
on macOS arm64. Each has separate Cargo target directories. Dependencies are
warmed once; each timed sample cleans only the consumer package, disables
incremental compilation, and performs a full consumer rebuild. Baseline and
candidate order alternates across five pairs. Initial warmups are excluded.
This is the script's `--rebuild consumer` mode. To measure changes in the
framework itself with third-party dependencies warm, use `--rebuild framework`:
each sample also cleans `xlfn` and `xlfn-macros` before rebuilding the consumer.

| Operation | Baseline median | Candidate median | Reduction |
| --- | ---: | ---: | ---: |
| `cargo check --offline` | 0.5402 s | 0.4426 s | 18.1% |
| `cargo build --offline` | 1.0999 s | 1.0061 s | 8.5% |

For the consumer build, Cargo frontend time fell from about 0.60 s to 0.50 s;
codegen remained about 0.43 s. This is a compilation benchmark of an `rlib`
consumer, not Windows XLL linking or Excel execution. Functions with small
bodies save less time. These results do not establish a comparable reduction in
cold dependency builds, release optimization, or incremental edit builds.

An initial full-feature framework build took 10.91 s in dev and 14.37 s in
release. Those single runs identified `syn` and the framework frontend as major
costs; they are not paired speedup evidence. Reusing tuple guard exit code was
also tested: dependency-warm framework rebuild medians were 3.934 s versus
3.931 s over five pairs. That change was discarded as indistinguishable from
noise.

Validation passed: 39 macro unit tests, the full-feature UDF compile-test suite
(including nested body macros and inner attributes), all-target/all-feature
Clippy for `xlfn` and `xlfn-macros`, and formatting. A Windows cross-target check
stopped in the unchanged BLAKE3 build script because `ml64.exe` was unavailable;
it did not complete framework validation. Native Windows timing remains
unmeasured.

## Reproduce

Export the baseline commit into a separate directory, then run from the
candidate repository:

```console
mkdir -p /tmp/xlfn-build-baseline
git archive c396e54be1ecb4dd8f3da5ac27c37daf35ae09f3 | tar -x -C /tmp/xlfn-build-baseline
python3 tools/measure_udf_build_time.py --baseline /tmp/xlfn-build-baseline --candidate . --output /tmp/xlfn-build-comparison
python3 tools/measure_udf_build_time.py --baseline /tmp/xlfn-build-baseline --candidate . --output /tmp/xlfn-build-release --profile release
python3 tools/measure_udf_build_time.py --baseline /tmp/xlfn-build-baseline --candidate . --output /tmp/xlfn-build-rtd --workload rtd
python3 tools/measure_udf_build_time.py --baseline /tmp/xlfn-build-baseline --candidate . --output /tmp/xlfn-build-handles --features handles
```

These commands compare all changes against the named commit; the recorded clean
measurements instead isolate serialization changes against the intermediate
snapshot described above. To exercise the earlier generated UDF workload, pass
`--workload generated --cache dependencies --incremental off --modes check build`.
For dependency-warm measurements of framework compilation, also pass
`--rebuild framework`. With `--cache clean`, all artifacts are removed regardless
of `--rebuild`. The handles command uses the same basic add-in fixture while
enabling the handles feature; it measures compilation of that feature graph.

Python 3.11+ and the pinned Rust toolchain are required. Dependencies must already
be available locally because measurements run offline. The script creates
isolated caches under `--output` and writes `results.json` there. Basic/RTD modes
build the standalone examples; generated mode also creates consumers under that
directory. Standalone lockfiles are resolved before timing.
Source hashes exclude nested `target` output directories. Every sample saves
Cargo's complete `all_units` data and the 20 units with the
longest durations in `slowest_units`, including dependencies and build scripts.
The existing `critical_units` field is retained for comparisons with earlier
reports. Unit durations can overlap and their sum is not build wall time;
`start` and `duration` identify which work overlaps the longest units. Only
units compiled in that sample appear in its timing report.
Use a fresh output directory for a new comparison. Set `--functions`,
`--statements`, and `--repeat` to compare other fixed workloads.
Keep sources unchanged during a run and avoid concurrent builds or tests.
