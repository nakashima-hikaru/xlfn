# Build, validate, and load

An XLL is not complete when `cargo check` succeeds. Excel loads a linked PE image, resolves imports, calls a fixed set of exports, and expects the image architecture to match the Excel process. xlfn therefore supplies `cargo xlfn check` and `cargo xlfn dist` as the supported artifact workflows.

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
6. stages configured native DLLs;
7. checks the import closure using the package's system-import policy.

Use Cargo build-selection flags normally:

```powershell
cargo xlfn check `
  --target x86_64-pc-windows-msvc `
  --features async `
  --locked
```

The command forces the static MSVC C runtime while preserving other `RUSTFLAGS`. This reduces deployment dependence on a separately installed VC runtime, but it does not make native DLL dependencies disappear.

## Release distribution

Build one target:

```powershell
cargo xlfn dist --target x86_64-pc-windows-msvc
```

Build both bitnesses as one output transaction:

```powershell
cargo xlfn dist --all
```

The default output root is `dist/`. A typical result is:

```text
dist/
├── win-x86/
│   ├── DeskTools.xll
│   ├── build-manifest.json
│   └── NativeEngine.dll
└── win-x64/
    ├── DeskTools.xll
    ├── build-manifest.json
    └── NativeEngine.dll
```

For `--all`, every target is built, staged, and verified before the previous output root is replaced. If the final replacement and its rollback both fail, the tool preserves the previous distribution in a recovery directory and reports that path. Do not delete the recovery directory until the failure has been investigated.

## Loading in Excel

Use **File -> Options -> Add-ins -> Manage: Excel Add-ins -> Go -> Browse**, then choose the XLL whose architecture matches the Excel process.

Keep the entire target directory together. The package verifier validates relative imports and
bundle files in that directory; moving only the `.xll` invalidates that deployment assumption.

When Excel reports that a file cannot be opened or is not a valid add-in, check these in order:

1. Excel process bitness versus XLL bitness;
2. whether the complete distribution directory was copied;
3. Windows file blocking and code-signing policy;
4. missing or wrong-architecture native dependencies;
5. diagnostics emitted during `xlAutoOpen`;
6. endpoint protection or application-control policy.

See [Troubleshooting](troubleshooting.md) for a symptom-oriented procedure.

## What successful validation proves

`cargo xlfn check` proves properties of the linked and staged bytes. It does not prove that:

- a worksheet function has correct business semantics;
- an unsafe native declaration matches the native binary;
- a library advertised as thread-safe actually is thread-safe;
- cancellation is timely;
- every supported Excel channel behaves identically;
- installation ACLs or code signatures are correct.

Treat linked-artifact validation, Rust tests, native ABI tests, and real-Excel qualification as separate release gates. The [Testing and release qualification](testing.md) chapter defines a complete matrix.
