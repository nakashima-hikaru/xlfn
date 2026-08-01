# Testing and release qualification

No single test layer is sufficient for an XLL. xlfn separates source correctness, ABI correctness, linked-artifact correctness, and real-Excel behavior so that evidence is not overstated.

## 1. Rust source checks

Run on every change:

```console
cargo fmt --all -- --check
cargo clippy --workspace --exclude excel-abi-probe --all-targets --all-features --locked -- -D warnings
cargo test --workspace --exclude excel-abi-probe --all-targets --all-features --locked
```

Also build the feature combinations your consumers use, including the no-default-feature facade:

```console
cargo check --package xlfn --locked
cargo check --package xlfn --features async --locked
cargo check --manifest-path examples/rtd-source/Cargo.toml --locked
```

Unit tests should cover pure business logic separately from generated boundaries. Property tests are useful for conversion limits, fingerprints, token parsing, array shapes, and cache keys.

## 2. Compile-fail contracts

Procedural macros and marker traits enforce misuse at compile time. Maintain compile-fail fixtures for:

- invalid context positions and borrowed context types;
- incompatible `thread_safe`, `macro_sheet`, reference, and async combinations;
- unsupported return modes;
- generic, unsafe, extern, or variadic UDFs;
- invalid defaults and argument names;
- handle producer restrictions;
- malformed native declarations.

A good diagnostic is part of the user interface. Assert relevant error text without overfitting compiler formatting.

## 3. Concurrency and shutdown tests

Deterministic tests should place barriers at lifecycle race points rather than relying only on stress loops. Cover:

- close racing with call entry;
- async cancellation versus worker claim and completion;
- task drop that re-enters cancellation state;
- handle replacement and formula-topic termination;
- RTD subscribe, publish, notify, `ServerTerminate`, and close barriers;
- worker queue-full shutdown and graceful drain;
- cache clear versus in-flight initialization;
- same-key cache recursion;
- native API and wrapper reentry rejection.

Use Loom or another model checker for small synchronization cores where practical, and retain ordinary stress tests for integration pressure.

## 4. Independent ABI probes

Rust declarations should be checked against C or C++ compiled with the target native or Excel headers.

For the repository's Excel SDK probe on Windows:

```powershell
$env:XLFN_SDK_INCLUDE = "C:\path\to\ExcelXllSdk\include"
cargo test --package excel-abi-probe --target x86_64-pc-windows-msvc --locked
cargo test --package excel-abi-probe --target i686-pc-windows-msvc --locked
```

A native adapter should have an equivalent probe for every shared structure, calling convention, and critical callback. Check `sizeof`, alignment, offsets, architecture-specific types, and a live trampoline call where possible.

Treat the downloaded SDK or header bundle as a supply-chain input: pin its digest and verify its publisher in CI.

## 5. Linked-artifact tests

On Windows:

```powershell
cargo xlfn check --target x86_64-pc-windows-msvc --all-features --locked
cargo xlfn check --target i686-pc-windows-msvc --all-features --locked
cargo xlfn dist --all --all-features --locked
```

Verify that:

- required lifecycle, COM, async, and UDF exports are present;
- x86 decorated exports are correct;
- PE machine type matches the target;
- every packaged import resolves;
- bundle files have unique case-insensitive basenames;
- the final staged bytes match the manifest records;
- a consumer crate can use the published facade under both targets.

Artifact tests do not start Excel.

## 6. Real-Excel qualification

Run the exact final package in each supported environment. At minimum, qualify each supported Excel bitness. When support claims include both Windows 10 and 11 or multiple enterprise channels, include those combinations explicitly.

Record:

```text
source commit:
package digest:
Windows edition/build:
Excel version/build/channel:
Excel bitness:
locale:
installation path/policy:
operator/date:
result and evidence:
```

### Lifecycle

- first load and registration;
- open failure containment;
- normal close;
- forced Excel termination followed by stale RTD registration recovery;
- unload/reload repeatedly in one process where supported;
- shutdown while work is queued or running.

### Ordinary functions

- scalar, string, Boolean, integer, error, date, and array round trips;
- blank versus missing policies;
- Function Wizard descriptions and help topics;
- thread-safe functions under multi-threaded recalculation;
- volatile and hidden registration behavior;
- wrong input types and propagated Excel errors.

### References

- same-sheet and sheet-qualified references;
- multi-area references;
- coordinate bounds and sheet names;
- owned coercion;
- 32-bit and 64-bit `IDSHEET` behavior.

### Handles

- create, consume, alias, recalculate, replace, and delete;
- wrong-type, stale, forged, and previous-session tokens;
- Formula Wizard, VBA/direct calls, and multi-cell caller rejection;
- workbook close and add-in unload cleanup;
- native object destruction on its required worker.

### Async

- immediate and delayed completion;
- cancellation before worker claim and while awaiting;
- late completion after cancellation;
- calculation end/cancel events;
- Excel close with queued and running work;
- error and panic containment.

### RTD

- one, two, three, and a larger batch such as 100 updates;
- number, Boolean, integer, string, error, and empty variants;
- `ConnectData`, `RefreshData`, `DisconnectData`, and `ServerTerminate`;
- blocked subscribe and notification during close;
- repeated publish and retry after a transient notification failure;
- unload/reload without stale callbacks.

### Native DLLs

- exact required and optional symbols;
- ABI mismatch and missing-symbol failure;
- wrong-architecture DLL rejection;
- native error strings and malformed outputs;
- maximum intended concurrency;
- context and object leak counters where the native exposes them;
- C++ exception containment on the native side.

## Release evidence

A high-quality release distinguishes these statuses:

- **implemented** — source exists;
- **unit-tested** — host tests passed;
- **Windows artifact-tested** — linked x86/x64 package inspection passed;
- **Excel validated** — named real-Excel environments passed;
- **signed/deployed** — final binaries passed organizational release controls.

Do not mark one status based on another. Publish the supported environment matrix and any known unqualified combinations with the release notes.
