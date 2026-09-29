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

## Release packaging

Create the directory that you will load or distribute:

```powershell
cargo xlfn package --target x86_64-pc-windows-msvc --locked
```

`package` uses the `release` profile by default. The resulting directory contains:

```text
package/win-x64/
├── AppTools.xll
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
