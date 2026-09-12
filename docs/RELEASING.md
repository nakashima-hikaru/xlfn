# Release procedure

Use this procedure to decide whether a candidate is ready for publication.
The [readiness record](RELEASE_READINESS.md) contains the current evidence;
the [compatibility policy](../guide/src/compatibility.md) defines the proposed
1.0 facade contract. Preparing a candidate does not publish it.

## Version and contract review

Record the exact candidate commit, features, compiler, and intended version
of each crate before collecting final release evidence.

| Crate | Current version | Contract to review |
| --- | --- | --- |
| `xlfn` | `0.2.0` | Documented facade and macros; core, async, handles, RTD |
| `cargo-xlfn` | `0.2.0` | CLI flags, defaults, exit status, metadata, package behavior |
| `xlfn-package` | `0.2.0` | Programmatic packaging API and build-manifest schema |
| `xlfn-sys` | `0.2.0` | Raw ABI crate; independently versioned, not the safe facade |
| `xlfn-common` | `0.1.0` | Internal shared conventions |
| `xlfn-macros` | `0.1.0` | Internal expansion implementation, accessed through `xlfn` |
| `xlfn-kernel` | `0.1.0` | Internal ownership and concurrency primitives |

The first four crates currently inherit one workspace version. An eventual
workspace version edit would affect all four. Decide their release versions
explicitly; the facade's 1.0 status must not accidentally announce a stable
standalone contract for another crate. Keep implementation crates in their
independent version domain. Update every affected exact path-dependency version
and standalone consumer lockfile together. A changed crate that is already
published needs a fresh version even when it is an implementation detail.

No version edits are part of the current preparation. The version-selection,
lockfile refresh, README installation examples, security support policy, and
final clean verification belong to a separately authorized release change.

Before freezing 1.0, review public signatures, exhaustive enums, public struct
fields, extension traits, feature combinations, auto traits, and macro inputs.
Macro expansion, CLI behavior, and workbook semantics need their own tests;
`cargo-semver-checks` cannot establish those contracts.

For `cargo-xlfn`, treat documented commands, options, defaults, metadata types,
success/failure behavior, and target-directory conventions as interfaces.
Human-readable console wording is not a machine-readable format. The
`build-manifest.json` format has an explicit schema version (currently 6);
incompatible format changes need a new schema and explicit rejection or
migration of unsupported schemas. Embedded `.xllexp` metadata and generated
wrappers must be validated together with the packaging tool. Neither Rust ABI
compatibility nor a cross-version link of separately compiled framework objects
is promised.

## Reproducible local checks

From a clean candidate checkout, using the pinned Rust toolchain:

```console
just check
just test-libtest
mdbook build guide
python3 -B guide/check.py
```

`just check` includes archive verification and strict rustdoc. The archive
check first runs `just release-metadata` to verify README/license presence,
license agreement with the repository originals, and exact internal version
pins. Its regression tests run as part of the same recipe. The archive
check is `cargo package`, with the non-publishable bindings generator excluded;
it does not upload anything. It verifies normalized manifests and builds the
packaged source. Do not use `--no-verify` for release evidence. Review the seven
archives under `target/package/`, including README and both license files.
For a working-tree review only, `--allow-dirty` permits archive verification;
this result must be repeated on the final clean candidate.

Also run `just miri-setup` and `just miri`, the dedicated ignored handle
close-race test from CI, and the Lean build/trace replay steps defined in
[CI](../.github/workflows/ci.yml). Retain results for default and selected
feature configurations as well as all-feature builds. Ignored performance
measurements are not passed correctness tests.

`just semver` currently compares with `0.1.0`. For crates now at `0.2.0`,
that comparison permits breaking changes and skips compatibility lints. A
successful result is not proof of a stable 1.0 API. Tag the actual first
stable source when it is released, then advance the baseline and require
checks within that compatibility line for subsequent changes. Do not force
`--release-type major` to hide failures. Preserve explicit review of the
unstable features and macro/CLI contracts when interpreting the report.

## Windows and Excel gates

Require successful CI from the exact final candidate for both
`i686-pc-windows-msvc` and `x86_64-pc-windows-msvc`:

- same-process tests, standalone examples, feature and lint checks;
- verification of the crate archives for each Windows target;
- linked basic and async/RTD fixture validation under static, dynamic, and
  inherited CRT policies;
- the pinned SDK-backed ABI probe;
- Miri, Lean, and replay of the Windows shutdown/handle traces.

Read the benchmark result for the same source and investigate relevant
regressions. A successful cross-compilation check on macOS or Linux is not
Windows execution evidence. Do not reuse a previous revision's successful CI
as approval of an edited candidate.

Run the [real-Excel qualification matrix](../guide/src/testing.md) on the exact
final packages. Record source commit, package digest, Windows build, Excel
build/channel/bitness, locale, operator/date, and result/evidence. At minimum,
cover both advertised Excel bitnesses and the features to be supported. Include
load/open, registration, conversion, MTR, handles, async, RTD, cancellation,
close, and unload/reload behavior. Record failures and unqualified environments
explicitly and narrow support claims if necessary. Do not mark blank cells as
passes or infer real-Excel behavior from PE inspection.

## Final decision

Publication may proceed only after all applicable gates above have evidence
for the same candidate, no known blocking defect remains, and the supported
environment matrix and version changes are reviewed. Missing evidence means
**hold**, not a successful qualification.

Archive verification does not prove registry ownership, version availability,
credentials, or publication acceptance. Check those separately when publication
is authorized. Dependency order is `xlfn-common` / `xlfn-kernel`, then
`xlfn-sys` / `xlfn-macros` / `xlfn-package`, then `xlfn` / `cargo-xlfn` (only
crates needing new versions). Verify registry resolution of the intended
versions and publish dependent crates only after their dependencies are
available.

After the authorized release, verify installation of `cargo-xlfn` and a
standalone add-in against registry dependencies, update the semver baseline,
and replace pre-release support wording in the README and security policy.
These publication and post-publication steps have not been performed here.
