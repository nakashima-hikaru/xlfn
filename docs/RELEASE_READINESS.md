# 1.0 release readiness

Assessment date: 2026-09-12. Source base: `4604db3` plus the preparation changes
in this working tree. All Cargo.toml versions remain unchanged; no crate has
been published by this preparation.

**Decision: hold publication pending final Windows CI and real-Excel
qualification.** The code and release checks have been prepared for review,
but this record does not certify a successful 1.0 release. Use the
[release procedure](RELEASING.md) to close the remaining gates and record the
final candidate commit and artifact digests.

## Scope prepared for 1.0

The [compatibility policy](../guide/src/compatibility.md) now states the proposed
stable facade, extension-trait and feature contracts, experimental exclusions,
minimum-toolchain policy, and source-versus-workbook compatibility boundaries.
The implementation crates retain an independent pre-1.0 version domain.

The preparation fixes these concrete issues:

- `XlValueRef` no longer publicly constructs or exposes raw `XLOPER12` values;
  custom converters use the safe value API. Compile-fail coverage protects the
  facade boundary. Code calling the former raw methods must migrate before 1.0.
- Borrowed arrays validate cell type tags before handing out infallible value
  views. This adds a linear tag scan; string and other payload conversion is
  still lazy and allocation-free until requested.
- `ExcelEnum` rejects names containing NUL or exceeding Excel's UTF-16 string
  limit during macro expansion, rather than accepting an unusable output name.
- CLI metadata rejects an incorrectly typed `xlfn` table or `artifact-name`
  instead of silently choosing defaults. The `test` and `bench` profiles find
  artifacts in Cargo's actual `debug` and `release` directories.
- Bundle paths reject embedded and trailing `.` components, as required by
  the documented path policy.
- All seven publishable crates include README and canonical MIT/Apache license
  texts. The internal kernel crate now has its own README. The
  `release-metadata` gate checks these files, license drift, and exact internal
  dependency versions, with six regression tests.
- CI verifies crate archives on Linux and both Windows targets and rejects
  rustdoc warnings. `just publish-check` now delegates to `cargo package` and
  cannot invoke publication.
- Feature documentation includes `handles` and `rtd`, explains their
  independence, and corrects the hidden macro-support path. Maintainer links
  point to existing evidence and are included in guide link validation.

## Verification evidence

The local host is Apple Silicon macOS with Rust `1.98.1`. Results below apply
to preparation checks, not to execution inside Excel. Final results are
recorded after the implementation changes; a clean candidate must repeat CI.

| Gate | Result | Evidence and limits |
| --- | --- | --- |
| Workspace tests, all features | Pass | 881 passed, 10 ignored; 65 compile-pass/fail fixtures within the integration tests; no runnable doc tests |
| Isolated CI test runner | Pass | `cargo nextest run --profile ci`: 881 passed, 10 skipped |
| Default and feature combinations | Pass | Separate core/handles/async/RTD test runs including compile contracts; cargo-hack depth-2 powerset, 36 configurations |
| Clippy, all targets/features, warnings denied | Pass | Repeated after all source changes |
| Dependency audit | Pass | Advisories, licenses, bans, sources checked with cargo-deny |
| API compatibility audit | Pass with limits | `xlfn-common`: 196 checks passed; `0.2.0` libraries skip lints against `0.1.0`; macros/CLI not covered |
| API documentation | Pass | Strict rustdoc on host and Windows x64 target |
| Crate archive verification | Pass, working tree | Seven publishable crates built after source/license changes with `cargo package --allow-dirty`; clean candidate rerun required |
| Windows source checking | Pass | Workspace, all features, both MSVC targets; no linking or Windows execution |
| Standalone consumers on the host | Pass | Locked checks for basic-xll, rtd-source, and xlfn-e2e-fixture on macOS |
| Windows all-target check on macOS | Environment limited | Criterion's C dependency requires Windows `malloc.h`; use Windows CI for tests/benches |
| Standalone consumers on Windows targets | Environment limited | Fresh basic-example build requires the unavailable MSVC `ml64.exe`; Windows consumer checks remain a CI gate |
| Generated Windows bindings | Pass | Regenerated; tracked bindings unchanged |
| Lean models and four checker executables | Pass | `lake build` plus explicit checker builds |
| Rust-to-Lean replay and dedicated handle model | Pass | Six ignored replay tests and one ignored Shuttle test passed; ten Lean fixtures accepted/rejected as expected |
| Miri | Pass with limits | 76 tests across kernel/handles/async/RTD, pinned `nightly-2026-08-22`; dependency provenance warnings described below |
| Formatting, panic inventory, metadata, guide | Pass | Formatting/whitespace, 46 reviewed panic references, 11 checker tests, metadata check, mdBook build, 29 guide chapters and maintainer links |
| Borrowed-array benchmark smoke | Completed | Existing raw-identity cases for 100/1,000/10,000 string cells ran; not a controlled before/after comparison |

Seven of the ten normally ignored tests were run separately as shown above;
the other three are manual performance measurements. Miri emitted
integer-to-pointer provenance warnings in `parking_lot_core` and
`crossbeam-epoch`. They did not fail the configured tests, but mean the run
must not be described as strict-provenance coverage of those dependencies.

The borrowed-array raw-identity smoke run measured approximately 4.44 µs,
53.7 µs, and 422.6 µs for 100, 1,000, and 10,000 string cells respectively.
These include identity hashing and ran alongside other checks on the host;
they do not isolate tag validation or establish a release performance budget.
The exact command was `cargo bench --package xlfn --bench argument_ingress
--features bench-internals --locked --
'^argument_ingress/matrix_string_(100|1k|10k)/raw_identity$'`.

The previously recorded remote CI at
[`96d6f56`](https://github.com/nakashima-hikaru/xlfn/actions/runs/34663909108)
failed in three jobs because the standalone RTD fixture called
`topic.parts().first()`. The source base already replaced that call with
`.next()`. This explains that failure but is not a passing CI result for the
current candidate. No remote workflow was dispatched during this preparation.

## Remaining release gates

| Required evidence | Status | Completion criterion |
| --- | --- | --- |
| Final clean candidate and version plan | Deferred by request | Select crate versions and update dependency constraints/consumer locks in an authorized release change; retain candidate commit |
| Exact-candidate Windows CI | Missing | Both architectures, archive builds, SDK ABI, linked XLL validation, Windows traces, and required quality jobs pass |
| Exact-candidate real-Excel matrix | Missing | Record both bitnesses, supported Windows/Excel builds, feature results, and package digests using the testing guide |
| Performance qualification | Smoke only | Review controlled candidate/baseline benchmark results, including borrowed-array admission's new linear tag validation |
| Registry and post-release validation | Not attempted | Check version availability/ownership when release is authorized, then validate registry-based consumers after publication |

No completed real-Excel execution record for this candidate was available in
the repository. Consequently Windows/Excel environment cells remain
**unqualified**. Do not claim tested Windows 10/11 and Excel build/channel
combinations until their evidence is attached. Once the required evidence is
complete, replace this hold decision with the reviewed decision, candidate
commit, and links to its immutable results.
