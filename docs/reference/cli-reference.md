# `cargo xlfn` reference

Use this page to look up command defaults, selection rules, and options. For a
step-by-step development loop, see [Build, validate, and load](../../guide/src/build-validation.md).

```console
cargo xlfn <COMMAND> [OPTIONS]
```

For the checkout-based tutorial, install the tool from the source tree as
shown in [Requirements](../../guide/src/requirements.md#obtain-the-source-checkout). Use
`cargo install cargo-xlfn --locked` when a matching published version is
available.

| Command | Default target selection | Default profile | Persistent output |
| --- | --- | --- | --- |
| `check` | Both Windows MSVC targets | `dev` | None |
| `package` | Requires `--target` or `--all` | `release` | Target directories under `package/` |

Both commands build the selected `cdylib` and verify staged bundle files, PE
architecture, required exports and `.xllexp` metadata, the effective CRT marker,
and the packaged DLL import closure.

## `cargo xlfn check`

```text
cargo xlfn check [PROJECT OPTIONS] [BUILD OPTIONS] [--target <TARGET>]
```

Without `--target`, `check` validates both `i686-pc-windows-msvc` and
`x86_64-pc-windows-msvc`. With it, only that target is validated. Staging is
temporary; use `package` to create files for deployment.

```console
cargo xlfn check --target x86_64-pc-windows-msvc
cargo xlfn check --package data-xlfn --profile release --locked
cargo xlfn check --manifest-path examples/basic-xll/Cargo.toml --all-features
```

The last example assumes the xlfn repository root as the current directory.

## `cargo xlfn package`

```text
cargo xlfn package (--target <TARGET> | --all) [--out <PATH>]
                 [PROJECT OPTIONS] [BUILD OPTIONS]
```

Exactly one of `--target` and `--all` is required.

| Option | Effect |
| --- | --- |
| `--target i686-pc-windows-msvc` or `--target x86` | Replaces `win-x86/` under the output root |
| `--target x86_64-pc-windows-msvc` or `--target x64` | Replaces `win-x64/` under the output root |
| `--all` | Builds both targets and replaces the complete output root |
| `--out <PATH>` | Selects the output root; default: `package/` |

The target aliases `x86` and `x64` also work with `check`.

```console
cargo xlfn package --all --locked
cargo xlfn package --target x64 --features async --out artifacts/addin
```

Each target directory contains `<artifact-name>.xll`, configured sidecar files,
and [`build-manifest.json`](cargo-metadata.md#build-manifestjson).

All selected targets are built, staged, and verified before replacement.
Replacement uses a transaction with best-effort rollback. It is not an atomic
swap visible to readers, and power-loss recovery is not guaranteed. The journal
is checked on the next invocation. If commit and rollback both fail, preserve
the reported recovery directory: it contains the previous package.

For `--all`, use a dedicated replaceable output directory. The current directory,
its ancestors, filesystem roots, and `.` are rejected. A single-target package
replaces only its target subdirectory, so the output root may contain other files.
See [Deployment](../../guide/src/deployment.md) before signing or installing the result.

## Project options

| Option | Meaning |
| --- | --- |
| `--manifest-path <PATH>` | Cargo manifest used for workspace discovery |
| `--package <NAME>` | Workspace member to build |

Package selection uses the first applicable rule:

1. `--package` selects that named member.
2. `--manifest-path` selects the package defined by that manifest. A virtual
   workspace manifest needs `--package` because it defines no package.
3. From a workspace member's directory or a subdirectory, that member is selected.
4. Otherwise, the workspace root package is selected when one exists.
5. A virtual workspace with one member selects its sole member. With multiple
   members, supply `--package` or a member's `--manifest-path`.

The selected package must define exactly one `cdylib` target.

## Build options

| Option | Meaning |
| --- | --- |
| `--crt <POLICY>` | `inherit`, `static`, or `dynamic`; default: `static` |
| `--target-dir <PATH>` | Base Cargo target directory, separated by CRT policy |
| `--profile <NAME>` | Overrides the command's default Cargo profile |
| `--features <A,B>` | Comma-separated feature selection |
| `--no-default-features` | Disables default features |
| `--all-features` | Enables all package features |
| `--locked` | Requires the existing lockfile |
| `--frozen` | Requires the existing lockfile and disables network access |
| `--offline` | Disables network access |

Qualify the same feature set that you distribute. The build manifest records
the selected `cdylib`'s actual features and the lockfile observed after its build.

An explicit `--crt` overrides `package.metadata.xlfn.crt`; see
[CRT policy](cargo-metadata.md#crt-policy) for enforcement and runtime imports.
Build output is isolated under `xlfn-crt-inherit`, `xlfn-crt-static`, or
`xlfn-crt-dynamic` beneath the selected Cargo target directory. Existing
`RUSTC_WRAPPER` and `RUSTC_WORKSPACE_WRAPPER` chains are preserved.

## Exit behavior and CI use

Build, staging, export, PE, import-closure, and commit failures return a non-zero
exit status. Human-readable console wording is not a machine-readable format.
A successful commit can report deferred backup cleanup with the retained
transaction path; preserve that path until cleanup is resolved.

A typical add-in CI workflow runs Rust checks, then artifact checks:

```console
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo xlfn check --package my-addin --all-features --locked
cargo xlfn package --package my-addin --all --all-features --locked --out package
```

Complete [real-Excel qualification](../EXCEL_TESTING.md) before distribution.
