# Testing and release qualification

No single test layer is sufficient for an XLL. xlfn separates source correctness, ABI correctness, linked-artifact correctness, and real-Excel behavior so that evidence is not overstated.

## 1. Rust source checks

Run on every change:

```console
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
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
- invalid application error conversions and adapter configuration.

A good diagnostic is part of the user interface. Assert relevant error text without overfitting compiler formatting.

## 3. Concurrency and shutdown tests

Deterministic tests should place barriers at lifecycle race points rather than relying only on stress loops. Cover:

- close racing with call entry;
- async cancellation versus application-adapter claim and completion;
- task drop that re-enters cancellation state;
- handle replacement and formula-topic termination;
- RTD subscribe, publish, notify, `ServerTerminate`, and close barriers;
- application-adapter queue-full shutdown and graceful drain;
- cache clear versus in-flight initialization;
- same-key cache recursion;
- application-adapter reentry rejection where the chosen implementation requires it.

Use Loom or another model checker for small synchronization cores where practical, and retain ordinary stress tests for integration pressure.

## 4. Independent ABI probes

Excel ABI declarations should be checked against code compiled with the target Excel headers. When an application adapter crosses another binary ABI, add an independent probe appropriate to that selected boundary.

For the repository's Excel SDK probe on Windows:

```powershell
$env:XLFN_SDK_INCLUDE = "C:\path\to\XlFnSdk\include"
cargo test --manifest-path probes/excel-abi-probe/Cargo.toml --features sdk-bindgen --target x86_64-pc-windows-msvc --locked
cargo test --manifest-path probes/excel-abi-probe/Cargo.toml --features sdk-bindgen --target i686-pc-windows-msvc --locked
```

An in-process binary adapter should have an equivalent probe for every shared structure, calling convention, ownership rule, and critical callback. Check `sizeof`, alignment, offsets, architecture-specific types, and a live trampoline call where possible. The commands above run those checks from the Rust test harness; a plain `cargo test` without `sdk-bindgen` is intentionally rejected rather than treated as a successful ABI check.

CI downloads the Microsoft Excel 2013 XLL SDK MSI and verifies its SHA-256 before extraction; local runs should use the same SDK release or an explicitly reviewed header bundle.

Treat the downloaded SDK or header bundle as a supply-chain input: pin its digest and verify its publisher in CI.

## 5. Historical benchmark tracking

Criterion benchmarks are tracked separately from correctness CI through
Bencher. The root `Justfile` is the shared command surface:

```console
just bench
just bench-async
just bench-sync
just bench-handle
just bench-check
```

The benchmark workflow runs on pushes to `main`, pull requests, and a nightly
schedule on the fixed `ubuntu-24.04` runner. It submits Criterion output using
Bencher's `rust_criterion` adapter and records the testbed as
`github-ubuntu-24.04`. The initial workflow is informational: it does not set
regression thresholds or fail on alerts. Thresholds should be enabled only
after the main-branch noise distribution is understood.

To enable history publishing, configure the repository with:

- an Actions secret named `BENCHER_API_KEY` containing a Bencher project API key;
- an Actions repository variable named `BENCHER_PROJECT` containing the Bencher project slug.

Fork pull requests never receive the secret. They run the benchmark command
without publishing results. Closed same-repository pull requests are archived
from Bencher so temporary PR branches do not accumulate indefinitely.

## 6. Linked-artifact tests

On Windows:

```powershell
cargo xlfn check --target x86_64-pc-windows-msvc --all-features --locked
cargo xlfn check --target i686-pc-windows-msvc --all-features --locked
cargo xlfn package --all --all-features --locked
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

## 7. Real-Excel qualification

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
- external object destruction on its required application-owned executor.

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

### External adapters

Adapt this matrix to the selected integration mechanism:

- required methods, symbols, endpoints, or protocol fields;
- version mismatch and missing-capability failure;
- wrong-architecture binary rejection where applicable;
- malformed outputs and bounded error conversion;
- authentication and authorization where applicable;
- maximum intended concurrency and overload behavior;
- context, object, connection, and process leak counters where available;
- exception, panic, crash, timeout, and disconnect containment at the adapter boundary.

## Release evidence

A high-quality release distinguishes these statuses:

- **implemented** — source exists;
- **unit-tested** — host tests passed;
- **Windows artifact-tested** — linked x86/x64 package inspection passed;
- **Excel validated** — named real-Excel environments passed;
- **signed/deployed** — final binaries passed organizational release controls.

Do not mark one status based on another. Publish the supported environment matrix and any known unqualified combinations with the release notes.
