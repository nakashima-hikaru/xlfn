# 1.0 candidate qualification

This is the maintainer gate for an application-facing `xlfn` 1.0 candidate.
The [compatibility contract](../guide/src/compatibility.md) defines its scope.
The manifests still declare the current pre-1.0 versions. Passing these checks
does not change versions or authorize publication.

## Automated gates

Use one candidate revision and retain the command output or CI run URL.
Uncommitted changes require a complete candidate snapshot; results for the
previous branch HEAD do not validate those changes.

| Area | Required evidence |
| --- | --- |
| Formatting, panic boundaries, lint, dependencies | `just fmt`, `just panic-boundaries`, strict workspace Clippy, `just deny` |
| Feature independence | `just features` checks all feature combinations; `just test-features` runs all 16 production combinations without benchmark/refinement helpers |
| Rust behavior and public extensions | `just test-ci` and `just test-libtest`, including macro pass/fail fixtures and cache/RTD/public configuration contracts |
| Guide and API reference | `just guide-check` compiles every Rust example or its standalone source project, checks 18 chapters and rendered links; `just doc` denies rustdoc warnings |
| Registry distribution | `just package-check`: seven crate archives, canonical license/readme files, exact support-crate pins, archive builds after removing local path dependencies |
| Concurrency and ownership | `just miri`, `just cache-release-ordering`, the dedicated Shuttle test in CI, and the Loom tests in the Rust suite |
| Formal model consistency | `just verus-audit`, `just verus`, Lean build/checker fixtures, and Rust-generated trace replay in CI |
| Windows integration without Excel | CI x86/x64 tests, SDK-backed ABI probe, standalone consumers, crate archive builds and XLL checks under static/dynamic/inherit CRT policies |

For a dirty development checkout, the equivalent archive check is:

```console
cargo package --workspace --exclude xlfn-windows-bindings-gen --all-features --locked --allow-dirty
```

`--allow-dirty` permits packaging the current files; it does not make their
provenance a clean commit. Use a reviewed candidate revision for release.

`just semver` compares against the historical `0.1.0` tag. Cargo semver checks
skip the pre-1.0 minor-version compatibility breaks for crates now at `0.2.0`.
That successful command is not a compatibility proof or the 1.0 API freeze.
Before publishing 1.0, review the documented facade, macro input/output behavior,
CLI metadata, build-manifest schema, and worksheet semantics together. After
the first 1.0 release, update the baseline to its actual immutable release tag
and retain behavioral tests alongside signature checks.

The formal worklists describe native atomic/lock/allocation identity and
cross-prover gaps. They remain research obligations, not completed proofs of
native execution. Release claims must respect
[the native refinement boundary](../verification/verus/NATIVE_ADAPTER_PLAN.md)
and [Lean's verification boundary](../formal/README.md#verification-boundary).
Miri's normal provenance mode and strict-provenance subsets are different
evidence; record dependency warnings instead of suppressing them.

## Excel and deployment-host gates

These require the actual deployment environment and remain separate from
automated Windows runners. Record the candidate commit, target, XLL and sidecar
hashes, Excel build/channel/bitness, calculation mode, and result for every run.

- Load, registration, worksheet function/error/Unicode/array behavior on the
  supported 32-bit and 64-bit Excel installations.
- Formula-handle reuse and invalidation; async completion/cancellation; RTD
  updates, backpressure and disconnect; normal close, reopen, and forced Excel
  termination followed by restart.
- Installation, CRT dependencies, signing/trust configuration, and unload rules
  for each distribution configuration actually offered.
- Controlled performance and resource measurements for representative real
  workloads. Review the local tradeoffs in [PERFORMANCE.md](PERFORMANCE.md),
  then use matching artifacts and the
  [Excel comparison runner](../benchmarks/excel-comparison/README.md).

## Publication step

Only after qualification and explicit publication authorization:

1. Choose the facade, raw ABI, packaging, CLI and support-crate versions under
   their independent contracts; update exact internal pins and consumer
   manifests/lockfiles together.
2. Repeat the metadata/archive checks and CI for that final versioned candidate.
   Requalify changed binaries in Excel; prior artifacts are not the release.
3. Publish dependencies before their dependants, verify the registry archives,
   then record immutable release tags and update the compatibility baseline.

There is no `cargo publish` in the check recipes. `publish-check` is only the
existing alias of the non-publishing `package-check`.
