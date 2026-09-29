# Build, validate, and load

After changing an add-in, validate its linked artifacts, create a package, and
load that package in Excel. Run the commands below from the add-in package
directory. For initial project setup, start with [Create your first
add-in](quick-start.md).

## Development validation

Choose the target for the **Excel process bitness**:

| Excel process | Rust target | Package directory |
| --- | --- | --- |
| 64-bit | `x86_64-pc-windows-msvc` | `package/win-x64/` |
| 32-bit | `i686-pc-windows-msvc` | `package/win-x86/` |

Validate one target during development:

```powershell
cargo xlfn check --target x86_64-pc-windows-msvc
```

A successful command builds the `cdylib`, stages its bundle files, and checks
the PE architecture, required exports, embedded framework metadata, CRT
policy, and import closure. It leaves no persistent deployment package.
`cargo check` alone does not perform these linked-artifact checks.

Omit `--target` to validate both architectures. Add the same feature and lock
options that you intend to distribute:

```powershell
cargo xlfn check --target x86_64-pc-windows-msvc --features async --locked
```

The default profile is `dev` and the default CRT policy is `static`.

`--features` accepts comma- or whitespace-separated feature names. Repeated
names are normalized in the build manifest. `--no-default-features`,
`--all-features`, `--locked`, `--offline`, and `--frozen` apply to both dependency
resolution and the actual build. Use `--package` to select a member of a virtual
workspace, or `--manifest-path` to select its manifest.

## Configure the package

Set the output basename and companion files in the add-in's `Cargo.toml`:

```toml
[package.metadata.xlfn]
artifact-name = "AppTools"
crt = "static"

[package.metadata.xlfn.bundle]
x86 = ["native/win-x86/engine.dll"]
x64 = ["native/win-x64/engine.dll"]
```

`artifact-name` defaults to the Cargo package name. Use a portable ASCII Windows
basename and omit the `.xll` extension, which the CLI adds. Configured bundle
paths are relative to the package manifest directory and are staged by basename
beside the XLL. Only list files your add-in actually needs. Basename collisions,
unknown settings, and malformed values are errors.

`crt` accepts `static`, `dynamic`, or `inherit`; `--crt` overrides the manifest.
The default `static` policy applies to target Rust crates. Companion DLLs retain
their own runtime requirements. With `dynamic`, deploy the matching Microsoft
VC runtime. `inherit` leaves the Rust CRT choice to the build configuration and
records the observed result. Build caches are separated by CRT policy.

Bundle `strict-paths` defaults to `true` and rejects linked path components.
Setting it to `false` allows links within the package directory; absolute paths,
parent traversal, and links escaping that directory are still rejected.
`external-imports = ["engine.dll"]`
declares dependencies supplied by the installation environment instead of the
bundle; it does not locate, install, or validate those external DLLs. Prefer
bundling dependencies when possible.

## Release packaging

Create the directory that you will load or distribute:

```powershell
cargo xlfn package --target x86_64-pc-windows-msvc --locked
```

`package` uses the `release` profile by default. The resulting directory contains:

```text
package/win-x64/
├── <artifact-name>.xll
├── build-manifest.json
└── configured sidecar files
```

Use `cargo xlfn package --all --locked` to produce both `win-x86` and `win-x64`.
Keep the feature options consistent between validation and packaging.

For signing, installation, and upgrades, continue with [Deployment and
distribution](deployment.md).

## Loading in Excel

1. Keep the complete target directory together, including its sidecars.
2. In Excel, open **File → Options → Add-ins → Manage: Excel Add-ins → Go → Browse**.
3. Select the packaged `.xll` matching the Excel process bitness.
4. Invoke a known worksheet function and verify its expected result.

After replacing an add-in, restart Excel so the test uses the new module.
If loading fails, start with [Excel refuses to load the
XLL](troubleshooting.md#excel-refuses-to-load-the-xll). If it loads but a formula
fails, use the [symptom index](troubleshooting.md#find-your-symptom).

## What successful validation proves

`cargo xlfn check` validates the linked and staged bytes. It does not establish
worksheet correctness, an external component's ABI or thread safety, timely
cancellation, installation trust, or behavior on a particular Excel build.

Before distributing an add-in, run your test suite and verify behavior directly
in Excel (see [Test your add-in](testing.md)).
