# Build, validate, and load

An XLL is not complete when `cargo check` succeeds. Excel loads a linked PE image, resolves imports, calls a fixed set of exports, and expects the image architecture to match the Excel process. xlfn therefore supplies `cargo xlfn check` and `cargo xlfn package` as the supported artifact workflows.

## Development validation

From the add-in package directory:

```powershell
cargo xlfn check
```

With no target selection, `check` validates both supported Windows targets. Select one during a focused development loop:

```powershell
cargo xlfn check --target x86_64-pc-windows-msvc
cargo xlfn check --target i686-pc-windows-msvc
```

`cargo xlfn check` does more than type checking. It:

1. builds and links the selected `cdylib`;
2. creates an isolated staging package;
3. reads the generated `.xllexp` manifest;
4. compares required lifecycle, COM, calculation-event, and UDF exports with the PE export table;
5. verifies the PE machine type against the requested target;
6. stages configured bundle files;
7. checks the import closure using the package's system-import policy.

Use Cargo build-selection flags normally:

```powershell
cargo xlfn check `
  --target x86_64-pc-windows-msvc `
  --features async `
  --locked
```

The default is `--crt static`, which reduces deployment dependence on a separately installed VC runtime. The command reports that default rather than changing the profile silently. Use `--crt dynamic` when the linked application and its binary dependencies require `/MD`, or `--crt inherit` to preserve Cargo, environment, and toolchain CRT settings exactly.

`static` and `dynamic` are enforced by an internal rustc wrapper only for the selected target; host build scripts and proc macros are unchanged. The linked XLL contains an effective-policy marker which `check` and `package` verify. The CRT observer recognizes an exact, case-insensitive allowlist of release/debug MSVC runtime DLLs and Universal CRT API-set DLLs; lookalike names are not classified. Under `static`, an observed dynamic CRT import is rejected because it commonly indicates that a prebuilt static library used `/MD`. Under `inherit`, the same static-Rust/dynamic-import combination is recorded and warned as potentially mixed.

CRT observation does not approve an external dependency. Any runtime DLL not included in the package must be listed explicitly in `external-imports`, where it remains a deliberate deployment exception and is not validated as part of the package closure.

The policy cannot recompile an existing `.lib`. Build linked binary components with a matching MSVC runtime: `/MT` (or `/MTd`) for `static`, and `/MD` (or `/MDd`) for `dynamic`. Matching CRT settings also do not make cross-module allocator ownership safe: allocate and free an object in the same module, or expose an explicit paired deallocator/caller-owned buffer contract.

## Release packaging

Build one target:

```powershell
cargo xlfn package --target x86_64-pc-windows-msvc
```

Build both bitnesses as one output transaction:

```powershell
cargo xlfn package --all
```

The default output root is `package/`. A typical result is:

```text
package/
├── win-x86/
│   ├── DeskTools.xll
│   ├── build-manifest.json
│   └── NativeEngine.dll
└── win-x64/
    ├── DeskTools.xll
    ├── build-manifest.json
    └── NativeEngine.dll
```

For `--all`, every target is built, staged, and verified before the previous output root is transactionally replaced. The replacement is not reader-visible atomicity; the journal provides crash recovery on the next invocation, while rollback remains best effort. If the final replacement and its rollback both fail, the tool preserves the previous package in a recovery directory and reports that path. Do not delete the recovery directory until the failure has been investigated.

## Loading in Excel

Use **File -> Options -> Add-ins -> Manage: Excel Add-ins -> Go -> Browse**, then choose the XLL whose architecture matches the Excel process.

Keep the entire target directory together. The package verifier validates relative imports and
bundle files in that directory; moving only the `.xll` invalidates that deployment assumption.

When Excel reports that a file cannot be opened or is not a valid add-in, check these in order:

1. Excel process bitness versus XLL bitness;
2. whether the complete package directory was copied;
3. Windows file blocking and code-signing policy;
4. missing or wrong-architecture binary dependencies;
5. diagnostics emitted during `xlAutoOpen`;
6. endpoint protection or application-control policy.

See [Troubleshooting](troubleshooting.md) for a symptom-oriented procedure.

## What successful validation proves

`cargo xlfn check` proves properties of the linked and staged bytes. It does not prove that:

- a worksheet function has correct business semantics;
- an application-defined ABI declaration matches an external binary;
- an external implementation advertised as thread-safe actually is thread-safe;
- cancellation is timely;
- every supported Excel channel behaves identically;
- installation ACLs or code signatures are correct.

Treat linked-artifact validation, Rust tests, application-adapter tests, and real-Excel qualification as separate release gates. The [Testing and release qualification](testing.md) chapter defines a complete matrix.
