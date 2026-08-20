# Testing and release qualification

No single test layer is sufficient for an XLL. Test the add-in's Rust code,
the linked Windows artifact, and the exact Excel environments that matter to
the deployment. Repository contributors should use the checks in
[CONTRIBUTING.md](../../CONTRIBUTING.md); this page focuses on add-in authors
and release operators.

## Rust and artifact checks

Run the add-in's unit and integration tests with its normal Cargo profile. For
the xlfn packaging contract, validate the exact target and feature selection:

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
- final staged bytes match the manifest records;
- a consumer crate can use the published `xlfn` crate under both targets.

Artifact tests do not start Excel. They complement, rather than replace, the
add-in's own unit tests and real-Excel qualification.

## Real-Excel qualification

Run the exact final package in every supported environment. At minimum,
qualify each supported Excel bitness. When support claims include multiple
Windows builds, Excel channels, or locales, include those combinations
explicitly.

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
- thread-safe, volatile, and hidden registration behavior;
- wrong input types and propagated Excel errors.

### References

- same-sheet and sheet-qualified references;
- multi-area references;
- coordinate bounds and sheet names;
- owned coercion;
- 32-bit and 64-bit `IDSHEET` behavior.

### Formula-owned handles

- create and consume a handle;
- alias an existing handle;
- recalculate with the same formula revision and confirm object reuse;
- change an explicit revision input and confirm a new object and token;
- retire the formula and confirm cleanup;
- reject wrong-type, stale, forged, and previous-session tokens;
- verify Formula Wizard, VBA/direct calls, and multi-cell caller behavior;
- verify workbook-close and add-in-unload cleanup;
- verify external object destruction on its required application-owned executor.

A stable token does not mean that the producer runs on every recalculation. The
same formula revision reuses its memoized object; revision changes create a new
object and token. Test observable object behavior or expose an explicit version
dependency instead of using token text as an application identifier.

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

Distinguish these statuses:

- **implemented** — source exists;
- **unit-tested** — host tests passed;
- **Windows artifact-tested** — linked x86/x64 package inspection passed;
- **Excel validated** — named real-Excel environments passed;
- **signed/deployed** — final binaries passed organizational release controls.

Do not mark one status based on another. Publish the supported environment
matrix and any known unqualified combinations with the release notes.
