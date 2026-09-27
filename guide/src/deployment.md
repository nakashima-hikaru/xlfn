# Deployment and distribution

Distribute a complete, versioned target directory. The release workflow is:
create the package, qualify it in Excel, sign the final files, and install them
in a controlled location. For the development build/load loop, see
[Build, validate, and load](build-validation.md).

## Produce target directories

From the add-in package directory:

```powershell
cargo xlfn package --all --out artifacts/addin-1.4.0 --locked
```

This produces `win-x86/` and `win-x64/` beneath the selected output root.
Distribute the directory matching the **Excel process bitness**, or supply both
with an installer that selects it correctly.

`--all` replaces the entire output root after staging and verification. Use a
dedicated directory containing no unrelated artifacts. See the
[CLI reference](https://github.com/nakashima-hikaru/xlfn/blob/main/docs/reference/cli-reference.md#cargo-xlfn-package) for single-target output,
transaction limits, and recovery behavior.

## Package contents

Each target directory contains:

- `<artifact-name>.xll`;
- configured sidecar files;
- `build-manifest.json` with build observations and artifact hashes.

Keep these files together. A sidecar's renamed or missing DLL can break both
explicit loading and transitive imports. All required non-system dependencies
must be packaged or covered by a documented external-import exception.

The [manifest reference](https://github.com/nakashima-hikaru/xlfn/blob/main/docs/reference/cargo-metadata.md#build-manifestjson) describes the
recorded fields. Its hashes support audit; they are not checked by a runtime
loader before DLL execution.

## Packaging boundary

Bundle configuration controls staging and PE dependency validation. Application
code owns runtime loading, symbol resolution, ABI/protocol checks, application
objects, and concurrency or cancellation guarantees. An add-in with no sidecars
needs no bundle metadata.

See [bundle metadata](https://github.com/nakashima-hikaru/xlfn/blob/main/docs/reference/cargo-metadata.md#bundle-metadata) to configure sidecars,
and [CRT policy](https://github.com/nakashima-hikaru/xlfn/blob/main/docs/reference/cargo-metadata.md#crt-policy) when linking native libraries.

## Installation location

Install into a directory whose contents cannot be replaced by unapproved users
or selected through workbook input. Use your organization's managed installation
and access-control policy.

Avoid temporary or Downloads directories, writable workbook-adjacent directories,
uncontrolled network shares, and search-path-dependent DLL placement. If the
application loads a sidecar, derive its explicit path from the installed package.
xlfn does not perform that load.

## Code signing

Signing is performed by your release system. Sign the XLL, executable sidecars
as permitted by their redistribution policy, and the installer or package container.

Verify signatures after the final byte-producing step. Signing changes file
hashes, so update your release audit metadata to describe the signed files.
Distribute the same bytes that were qualified and approved by your release process.
See [Installation location](#installation-location) for where to place the signed package.

## External imports

An external import is a deployment promise: the environment supplies that DLL.
Document who installs it, its version and bitness, and how it is found. Test the
complete installation on a clean machine.

Use [the metadata rules](https://github.com/nakashima-hikaru/xlfn/blob/main/docs/reference/cargo-metadata.md#external-imports) for explicit
exceptions and [CRT policy](https://github.com/nakashima-hikaru/xlfn/blob/main/docs/reference/cargo-metadata.md#crt-policy) for automatically
recognized runtime imports. Do not exempt a missing application DLL merely to
pass validation.

## Versioning worksheet APIs

A workbook depends on function names, argument semantics, enum text, handle
producers, and RTD identities. Follow the
[workbook compatibility guidance](https://github.com/nakashima-hikaru/xlfn/blob/main/docs/reference/compatibility.md#workbook-compatibility)
when changing those contracts.

For a breaking worksheet change, provide a new Excel name or an explicit migration
plan. If an old function remains during migration, document its purpose and removal
schedule. Framework implementation versioning does not migrate deployed workbooks.

## Upgrade and rollback

Do not overwrite a loaded XLL in place. For an upgrade:

1. Close every Excel process using the add-in and check for background processes.
2. Install the complete new target directory, preferably under a versioned path.
3. Retain the previous signed directory for rollback.
4. Load the new version and run its smoke tests.
5. Remove old versions after the rollback window.

Changes to executable sidecars, application protocols, handle token semantics,
or RTD ownership require an Excel restart. For close failures, use
[Excel hangs during close](troubleshooting.md#excel-hangs-during-close).

## Distribution checklist

Before distributing a package:

- Build with `--locked` from a clean source revision and record the toolchain and features.
- Qualify each supported architecture and Excel environment using [Testing](testing.md).
- Review bundle dependencies, external-import exceptions, and the build manifest.
- Verify final signatures and deployment access controls.
- Test first install, upgrade, rollback, and restart on a clean machine.
- Archive the exact package, its digest, and qualification evidence.
