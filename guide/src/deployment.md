# Deployment and distribution

A deployable add-in is a versioned directory, not an isolated `.xll` file. Build, validate, sign, and install the XLL together with every packaged sidecar required by the application and its audit manifest.

## Produce target directories

```powershell
cargo xlfn package --all --locked
```

This creates `win-x86` and `win-x64` directories under the selected output root. Distribute only the directory matching the Excel process bitness, or package both with an installer that selects correctly.

Use an explicit output root for release automation:

```powershell
cargo xlfn package --all --out artifacts/xlfn-1.4.0 --locked
```

The `--all` operation stages and validates both targets before replacing the output root. Do not point it at the repository root, current directory, or a directory that contains unrelated artifacts.

## Package contents

Each target directory contains:

- `<artifact-name>.xll`;
- configured sidecar files and their packaged PE dependencies;
- `build-manifest.json`.

The manifest records schema version 6, package and artifact identity, target, profile, selected
features, requested and observed CRT policy, configured/resolved bundle sources, import-policy
version, file sizes, and SHA-256 values. Its integrity section explicitly states that hashes are
audit metadata and are not verified before DLL execution.

Keep all files together. Renaming a sidecar DLL or moving it to another directory can break both explicit loading and transitive imports.

## Packaging boundary

Bundle staging and PE dependency validation are build and distribution facilities. They do not load a sidecar at runtime, resolve application symbols, choose an ABI or protocol, construct application objects, or prove that downstream calls are thread-safe or cancellable. If application code uses a packaged component, that code owns the runtime contract and loading policy.

If the add-in has no sidecar files, no bundle metadata is required.

## Versioning worksheet APIs

A released workbook depends on more than the crate's semantic version. Treat these as public contracts:

- Excel-visible function names;
- UDF IDs and generated export identities;
- argument order, names, defaults, blank/missing policy, and accepted types;
- enum strings;
- handle object types and producer semantics;
- RTD topic identity;
- add-in ID and category.

Adding a new function is usually compatible. Renaming a function, changing argument order, changing a default, or changing an enum text can silently alter existing workbooks.

For breaking worksheet changes, prefer a new Excel name or an explicit versioned function while the old function remains as a documented compatibility layer for one migration window. Do not leave undocumented shims indefinitely.

## Installation location

Install into a directory that ordinary workbook input cannot select and unprivileged users cannot replace after approval. Appropriate enterprise mechanisms include a managed per-user directory with restricted ACLs or an administrator-controlled application directory.

Avoid:

- Downloads and temporary directories;
- workbook-adjacent writable directories;
- network shares without a deliberate trust policy;
- search-path-dependent DLL placement;
- copying only the XLL while resolving application sidecars from an ambient global directory.

If application code loads a packaged DLL, it should use an explicit path derived from the installed package rather than ambient search paths. xlfn does not perform that load. Transitive dependencies still resolve according to Windows loader behavior and the validated package import closure.

## Code signing

Authenticode signing is intentionally external to xlfn because keys, hardware security modules, timestamps, and enterprise trust policy are deployment concerns. Sign:

- the XLL;
- every first-party executable sidecar;
- third-party executable sidecars when redistribution terms and signing policy permit;
- installers or package containers.

Verify signatures after the final byte-producing step. Signing changes the file hash, so generate or update release audit metadata in the order required by your release system. Do not sign one set of bytes and distribute another.

## External imports

The package verifier recognizes a versioned default set of Windows system DLLs and API-set names. A non-packaged import outside that set fails validation unless its basename appears in `external-imports`.

An external import is an explicit deployment exception, not a general bypass. Use it only for a component guaranteed by the target environment, document who installs it, and test on a clean machine.

```toml
[package.metadata.xlfn.bundle]
external-imports = ["approved-inbox-component.dll"]
```

Do not add a missing application dependency to `external-imports` merely to pass the verifier.

## Upgrade and rollback

Do not overwrite a loaded XLL in place. Excel can retain module and DLL file handles until the process exits. A reliable upgrade procedure is:

1. close every Excel process using the add-in;
2. verify that no background Excel process remains;
3. install the complete new target directory transactionally with best-effort rollback, or publish it under a versioned path;
4. preserve the previous signed directory for rollback;
5. load the new version and run smoke tests;
6. remove old versions only after the rollback window.

When the add-in's executable sidecars, application-owned data or protocol assumptions, token semantics, or RTD ownership schema change, restart Excel rather than attempting an in-process hot swap.

## Distribution checklist

Before publishing:

- build with `--locked` from a clean checkout;
- record source commit, toolchain, target, and feature set;
- run both artifact and real-Excel qualification gates;
- review `build-manifest.json`, its effective bundle policy, and staged bundle paths;
- verify x86/x64 architecture independently;
- sign and verify every executable binary;
- scan the final package with organizational security tooling;
- install from the exact final package on a clean test machine;
- archive release evidence and the rollback package.
