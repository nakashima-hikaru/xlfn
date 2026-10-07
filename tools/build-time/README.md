# Consumer build time

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
```

These commands compare all changes against the named commit; the recorded clean
measurements instead isolate serialization changes against the intermediate
snapshot described above. To exercise the earlier generated UDF workload, pass
`--workload generated --cache dependencies --incremental off --modes check build`.

Python 3.11+ and the pinned Rust toolchain are required. Dependencies must already
be available locally because measurements run offline. The script creates
isolated caches under `--output` and writes `results.json` there. Basic/RTD modes
build the standalone examples; generated mode also creates consumers under that
directory. Standalone lockfiles are resolved before timing.
Use a fresh output directory for a new comparison. Set `--functions`,
`--statements`, and `--repeat` to compare other fixed workloads.
Keep sources unchanged during a run and avoid concurrent builds or tests.
