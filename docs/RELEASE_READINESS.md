# 1.0 release readiness

Assessment date: 2026-09-26. Source base: `4871fdb` plus the preparation
changes in this working tree. Package version numbers are unchanged and no
publication has been performed.

**Decision: hold publication.** Final-candidate Windows CI, real-Excel
qualification, controlled performance qualification, and release-version
selection still need evidence. Local checks prepare the source for that
decision; they do not certify a 1.0 release.

The [API stability policy](API_STABILITY.md) defines the intended
stable application contract. The [release procedure](RELEASING.md) describes
how to collect evidence for an exact candidate. The
[2026-09-12 assessment](archive/release-readiness/2026-09-12.md) is historical
evidence, not a current-candidate pass.

## Preparation in this revision

| Contract | Preparation | Acceptance evidence |
| --- | --- | --- |
| Independent release domains | Every member declares its current version explicitly; the facade, raw ABI, packaging API, and CLI no longer inherit a shared version | Release-metadata regression rejects version inheritance for publishable crates; resolved package versions remain unchanged |
| Supported feature set | The documented all-capabilities dependency includes `cache` together with `async`, `handles`, and `rtd` | Feature combinations and guide checks |
| Configuration API | `AsyncConfig` is the concrete type; redundant runtime configuration aliases and setters are removed | External consumer tests cover const construction, feature gating and combined configuration; migration notes identify replacements |
| Numerical output | Scalar, collection, builder and custom-converter outputs share the finite-number check and `#NUM!` error | Regressions cover NaN and both infinities across output shapes, including builder recovery after rejection |
| Reference extensions | Generated wrappers dispatch to the declared `FromExcelReference` type; raw admission errors retain the argument name | Owned and borrowed downstream implementations compile; reference-based new-handle creation remains rejected |
| RTD extensions | Candidate conversion, publication validation and source-handle lifetime are documented separately | Custom converters cannot publish non-finite numbers or strings exceeding the UTF-16 limit; a valid value can follow rejection |
| Cache callbacks | Key identity and callback reentry restrictions are explicit, including their distinction from deferred destruction | Public Rust documentation and the caching guide agree with the lookup, initialization and reclamation paths |
| Build provenance | Package manifests take the library path and enabled features from the selected Cargo compiler artifact, and hash the lockfile after each build | A multi-member workspace regression distinguishes metadata feature unification from actual package build features, including fresh artifacts and lockfile updates |

This revision builds on the committed corrections for initialization failure
quarantine, custom collection conversions, content-based borrowed-string
equality, cache registration cleanup, and deferred key/value destruction.
Their regression tests remain part of the workspace suite. They do not remove
the explicit application quiescence contract for physical DLL unloading.

## Local verification

The local environment is Apple Silicon macOS with Rust `1.98.1`. These are
working-tree results, including the concurrent cache endpoint/benchmark work
present during validation. They are not evidence for a clean release commit.

| Check | Result |
| --- | --- |
| Workspace all-feature tests, serialized libtest | 964 passed, 0 failed, 11 ignored across 20 suites, including doctests and downstream compile contracts |
| Strict Clippy, workspace/all targets/all features | Passed with `-D warnings` |
| Feature combinations | All 28 checks from `just features` passed |
| Strict workspace rustdoc | Passed with `RUSTDOCFLAGS="-D warnings"` |
| Standalone consumers | `basic-xll`, `rtd-source`, and `xlfn-e2e-fixture` passed host `cargo check --locked`; no Windows DLL execution is implied |
| Release metadata | Checker and 7 regression tests passed; all 8 workspace member versions match the source base |
| Package archives | All 7 publishable crates packaged and rebuilt with `--all-features --locked --allow-dirty`; archived README/licenses match their originals, normalized versions and exact internal dependency pins are correct, and path/workspace dependency declarations are removed |
| Panic boundaries | 50 reviewed direct references; checker and 5 regression tests passed |
| Dependency policy | `cargo deny check` passed advisories, bans, licenses and sources |
| User guide | All 29 chapters passed validation; mdBook built successfully |
| Formatting and patch hygiene | `cargo fmt --all -- --check` and `git diff --check` passed |

Ignored tests are not counted as passes. The dedicated Miri, formal proof and
ignored model/replay suites have not been rerun for this preparation; earlier
results do not qualify the final candidate. Working-tree archive verification
must also be repeated on the selected clean release candidate.

## Remaining release gates

| Required evidence | Status | Completion criterion |
| --- | --- | --- |
| Final clean candidate and independent versions | Not selected | Review intended versions per crate, exact dependency pins and consumer locks; record the clean source commit |
| Windows CI for that candidate | Missing | Both MSVC architectures, same-process tests, standalone consumers, archive builds, SDK ABI, linked XLL checks and formal trace replay pass |
| Real-Excel qualification for the same packages | Missing | Record package hashes, Windows/Excel versions and bitness, locale, operator/date, feature behavior and unload/reload results using the testing guide |
| Controlled performance qualification | Incomplete | Review candidate/baseline results on the deployment workload, including conversion, RTD, handles and cache pressure; local microbenchmarks alone do not qualify Excel performance |
| Stable API baseline and release notes | Pending release selection | Review Rust and macro contracts, CLI/package behavior and workbook-visible changes; record the first stable source as the future compatibility baseline |
| Registry and post-release checks | Not attempted | Once publication is authorized, verify ownership and version availability, publish in dependency order, and validate registry-based consumers |

`cargo-semver-checks` against `0.1.0` skips compatibility lints for the current
`0.2.0` packages. A successful historical comparison is not evidence that their
future 1.0 contract is stable. Macro behavior, extension traits, package schema,
and workbook-visible errors require their own contract tests and review.

No completed real-Excel record exists in this preparation. Windows/Excel
environment cells remain **unqualified** until linked evidence identifies the
same candidate and package digests. Cross-target Rust checking and PE inspection
do not replace native Windows or Excel execution.
