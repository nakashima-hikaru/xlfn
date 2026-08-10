# `cargo xlfn` reference

`cargo-xlfn` is an optional developer tool that validates linked XLL artifacts and produces bitness-specific package directories. Invoke it through Cargo:

```console
cargo xlfn <COMMAND> [OPTIONS]
```

Install `cargo-xlfn` from crates.io:

```console
cargo install cargo-xlfn --locked
```

Or from a local checkout:

```console
cargo install --path crates/cargo-xlfn --locked --force
```

## `cargo xlfn check`

```text
cargo xlfn check [PROJECT OPTIONS] [BUILD OPTIONS] [--target <TARGET>]
```

Without `--target`, the command validates both supported targets:

```text
i686-pc-windows-msvc
x86_64-pc-windows-msvc
```

With `--target`, it validates only the selected target.

For each target, `check`:

1. runs `cargo build` for the selected package and build configuration;
2. requires exactly one `cdylib` target;
3. stages configured bundle files in a temporary package;
4. copies the linked DLL as `<artifact-name>.xll`;
5. verifies required XLL exports and the generated `.xllexp` manifest;
6. verifies the embedded effective Rust CRT policy and direct dynamic CRT imports;
7. verifies PE architecture;
8. validates the complete packaged DLL import closure.

The default Cargo profile is `dev` unless `--profile` is supplied. `check` does not create a persistent package directory.

Examples:

```console
cargo xlfn check
cargo xlfn check --target x86_64-pc-windows-msvc
cargo xlfn check --target x86_64-pc-windows-msvc --crt dynamic
cargo xlfn check --package data-xlfn --profile release --locked
cargo xlfn check --manifest-path xlfn/examples/basic-xll/Cargo.toml --all-features
```

## `cargo xlfn package`

```text
cargo xlfn package (--target <TARGET> | --all) [--out <PATH>]
                 [PROJECT OPTIONS] [BUILD OPTIONS]
```

Exactly one of `--target` and `--all` is required.

- `--target i686-pc-windows-msvc` writes an x86 package.
- `--target x86_64-pc-windows-msvc` writes an x64 package.
- `--all` stages both architectures and transactionally replaces the output root with best-effort rollback.
- `--out <PATH>` selects the output root; the default is `package`.

`package` uses the `release` profile by default unless `--profile` is supplied.

Typical output:

```text
package/
├── win-x86/
│   ├── MyAddin.xll
│   ├── build-manifest.json
│   └── packaged sidecar files
└── win-x64/
    ├── MyAddin.xll
    ├── build-manifest.json
    └── packaged sidecar files
```

Each target is fully staged and validated before commit. With `--all`, either both target directories are committed or the previous package is restored when rollback is possible. This is a transactional replacement, not a reader-visible atomic directory swap: readers may observe the replacement window, and power loss is outside the guarantee. The transaction journal is checked on the next invocation. If commit and rollback both fail because of a filesystem fault, the command reports a preserved recovery path rather than deleting the previous package.

Examples:

```console
cargo xlfn package --all
cargo xlfn package --target x86_64-pc-windows-msvc
cargo xlfn package --all --out artifacts/xll --locked
cargo xlfn package --target i686-pc-windows-msvc --features async
```

For `--all`, the output root must be a dedicated directory; `.` and filesystem roots are rejected because the whole root is replaced transactionally. A single-target package replaces only its `win-x86` or `win-x64` subdirectory under `--out`, so an existing parent directory may be used.

## Project options

| Option                   | Meaning                                     |
| ------------------------ | ------------------------------------------- |
| `--manifest-path <PATH>` | Cargo manifest used for workspace discovery |
| `--package <NAME>`       | workspace package to build                  |

When no package is supplied, the workspace root package is selected. A virtual workspace or an ambiguous workspace requires `--package`.

## Build options

| Option                  | Forwarded behavior                      |
| ----------------------- | --------------------------------------- |
| `--crt <POLICY>`        | `inherit`, `static`, or `dynamic`; see below |
| `--target-dir <PATH>`   | base target directory, separated by CRT policy |
| `--profile <NAME>`      | Cargo profile                           |
| `--features <A,B>`      | comma-separated feature selection       |
| `--no-default-features` | disable default features                |
| `--all-features`        | enable all package features             |
| `--locked`              | require the existing lock file          |
| `--frozen`              | require lock file and no network access |
| `--offline`             | disable network access                  |

Feature flags affect both the binary and its expected export manifest. Always qualify the same feature set that will be distributed.

The CRT policy defaults to `static`, can be set persistently with
`package.metadata.xlfn.crt`, and is overridden by an explicit `--crt`:

- `inherit` leaves `RUSTFLAGS`, `CARGO_ENCODED_RUSTFLAGS`, Cargo configuration,
  and wrappers untouched;
- `static` enforces `+crt-static` for Rust invocations targeting the selected
  MSVC triple;
- `dynamic` enforces `-crt-static` for those target invocations.

Build output is isolated under `xlfn-crt-inherit`, `xlfn-crt-static`, or
`xlfn-crt-dynamic` beneath the selected Cargo target directory. Existing
`RUSTC_WRAPPER` and `RUSTC_WORKSPACE_WRAPPER` chains are preserved.

## Exit behavior and CI use

The command returns a non-zero exit status on build, staging, export, PE, import-closure, or commit failure. Treat any failure as a release-blocking artifact failure.

A representative CI sequence is:

```console
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo xlfn check --package my-addin --all-features --locked
cargo xlfn package --package my-addin --all --all-features --locked --out package
```

`cargo xlfn check` complements rather than replaces Rust tests and real-Excel qualification.
