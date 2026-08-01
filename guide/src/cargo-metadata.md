# Cargo metadata reference

`cargo-xlfn` reads package-specific settings from `Cargo.toml`. Paths are interpreted relative to the selected package's manifest directory, not the workspace root or current shell directory.

## Basic metadata

```toml
[package.metadata.xlfn]
artifact-name = "DataTools"
```

`artifact-name` controls the distributed XLL basename:

```text
DataTools.xll
```

When omitted, the Cargo package name is used.

The value must be a valid Windows basename. It must:

- be non-empty;
- contain no control characters or `< > : " / \\ | ? *`;
- not end with a dot or space;
- not use a reserved device stem such as `CON`, `PRN`, `AUX`, `NUL`, `COM1` through `COM9`, or `LPT1` through `LPT9`.

Do not add the `.xll` extension to `artifact-name`; the tool supplies it.

## Bundle metadata

List sidecar files by target:

```toml
[package.metadata.xlfn.bundle]
x86 = [
    "native/x86/NativeEngine.dll",
    "native/x86/NativeSupport.dll",
]
x64 = [
    "native/x64/NativeEngine.dll",
    "native/x64/NativeSupport.dll",
]
external-imports = ["OrganizationRuntime.dll"]
strict-paths = true
```

| Key | Type | Meaning |
|---|---|---|
| `x86` | array of strings | files packaged for `i686-pc-windows-msvc` |
| `x64` | array of strings | files packaged for `x86_64-pc-windows-msvc` |
| `external-imports` | array of strings | approved non-system DLL basenames supplied outside the package |
| `strict-paths` | Boolean | reject symbolic links/reparse points in configured source paths |

Unknown fields are rejected.

### Bundle path rules

Every configured native path must:

- be a non-empty relative path;
- contain only normal path components—no root, drive prefix, `.` or `..`;
- resolve to a regular file;
- canonicalize within the package manifest directory;
- have a case-insensitively unique output basename;
- not collide with `<artifact-name>.xll` or `build-manifest.json`.

With `strict-paths = true`, each configured component is also rejected when it is a symbolic link or Windows reparse point. Keep this enabled for release inputs. A relaxed setting still enforces canonical containment but permits links; use it only for a controlled development workflow whose trust boundary is documented.

The output package is flat. Directory structure in configured paths is not preserved, so basename uniqueness is mandatory.

### External imports

An entry in `external-imports` must be a DLL basename such as:

```toml
external-imports = ["OrganizationRuntime.dll"]
```

Paths and non-DLL names are rejected. Matching is case-insensitive.

This option is an explicit deployment exception: the dependency need not be packaged because the deployment environment promises to resolve it. Do not list a missing native DLL merely to make validation pass. Record who installs the dependency, where it is loaded from, how it is versioned, and how its bitness is controlled.

Windows system imports are accepted by the versioned built-in `windows-system-v1` policy. Every other direct or transitive import must resolve to a packaged basename or an approved external import.

## Full example

```toml
[package]
name = "data-xlfn"
version = "1.4.0"
edition = "2024"
rust-version = "1.97.1"

[lib]
crate-type = ["cdylib"]

[dependencies]
xlfn = { version = "0.1", features = ["async"] }

[package.metadata.xlfn]
artifact-name = "DataTools"

[package.metadata.xlfn.bundle]
x86 = [
    "native/x86/NativeEngine.dll",
    "native/x86/NativeMath.dll",
]
x64 = [
    "native/x64/NativeEngine.dll",
    "native/x64/NativeMath.dll",
]
external-imports = []
strict-paths = true

[profile.release]
panic = "unwind"
lto = "thin"
codegen-units = 1
```

The selected package must contain exactly one `cdylib` target.

## `build-manifest.json`

Every distribution directory contains schema version 3 audit metadata. Its top-level fields are:

| Field | Meaning |
|---|---|
| `schema` | manifest schema number |
| `package` | Cargo package name |
| `package_version` | Cargo package version |
| `artifact` | configured artifact basename |
| `target` | Rust target triple |
| `profile` | Cargo profile |
| `features` | selected package feature set |
| `native_sources` | configured paths and resolved source paths used during staging |
| `system_import_policy` | versioned system-DLL policy and approved external imports |
| `integrity` | explicit trust-boundary statement |
| `files` | relative path, byte size, and SHA-256 for every distributed file |

The integrity block deliberately states that hashes are **audit metadata only** and are not verified before native code executes. Windows loads and initializes a DLL before application-level ABI checks can run. Use access-controlled installation directories and code signing for runtime trust; do not treat the JSON file as a secure loader.

## Metadata review checklist

Before release:

1. compare the two architecture lists rather than assuming they are symmetric;
2. inspect every transitive import reported by `cargo xlfn check`;
3. keep `external-imports` empty unless deployment owns the exception;
4. review resolved source paths in the manifest for unintended build-machine locations;
5. verify output basenames and signatures after staging;
6. archive the manifest with release evidence, but do not use it as the sole integrity control.
