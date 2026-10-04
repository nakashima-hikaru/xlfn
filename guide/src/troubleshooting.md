# Troubleshooting

Start with the earliest failing boundary. Do not debug a worksheet result before confirming that the correct XLL loaded and its dependencies resolved.

## Find your symptom

| Symptom | First check | Detailed procedure |
| --- | --- | --- |
| Excel refuses the XLL | Excel bitness and the complete package directory | [Load failure](#excel-refuses-to-load-the-xll) |
| Add-in loads, function is absent | Registration diagnostics and the loaded module version | [Missing functions](#the-add-in-loads-but-functions-are-missing) |
| `#VALUE!` | Argument type and blank/missing policy | [Value errors](#a-cell-shows-value) |
| `#NUM!` | Finite values and numeric range | [Numeric errors](#a-cell-shows-num) |
| `#N/A` | Handle/session state or an unavailable result | [Unavailable results](#a-cell-shows-na) |
| Handle appears unchanged | Whether the formula revision changed | [Handle refresh](#a-handle-does-not-appear-to-refresh) |
| Async formula never completes | Cancellation and blocked workers | [Async completion](#an-async-formula-never-completes) |
| RTD does not update | Subscription lifetime and publish errors | [RTD updates](#rtd-does-not-update) |
| External adapter fails | Its diagnostics, ABI, and packaged dependencies | [Adapter initialization](#external-adapter-fails-to-initialize) |
| Excel hangs on close | Work still running or a held lock | [Close hangs](#excel-hangs-during-close) |
| Packaging fails | The reported path or unresolved import | [Package validation](#cargo-xlfn-package-refuses-paths-or-imports) |

Before escalating, [collect the environment and diagnostic evidence](#collect-basic-evidence).

## Collect basic evidence

Record:

```text
add-in version and source commit:
Windows version:
Excel version/channel and bitness:
XLL path and architecture:
feature set:
exact formula:
cell result or Excel dialog text:
diagnostic ID and relevant log lines:
reproduction after a clean Excel restart:
```

The built-in diagnostic log is normally at:

```text
%LOCALAPPDATA%/<addin-id>/logs/diagnostics.log
```

Do not publish logs without reviewing them for sensitive installation or business data.

## Excel refuses to load the XLL

Check:

1. XLL architecture matches the Excel process, not merely Windows;
2. the file is the staged `.xll`, not the original Cargo `.dll`;
3. every file from the target distribution directory is present;
4. the package was not copied from an untrusted source and blocked by Windows policy;
5. required signatures are valid;
6. endpoint protection did not quarantine a dependency;
7. the package passed `cargo xlfn check` on the same target.

Rebuild explicitly:

```powershell
cargo xlfn check --target x86_64-pc-windows-msvc --locked
```

Use the x86 target for 32-bit Excel.

See [Build, validate, and load](build-validation.md#loading-in-excel) for the
normal loading procedure and [Deployment](deployment.md) for installation policy.

## The add-in loads but functions are missing

- Confirm that exactly one `#[excel_addin]` is at crate root.
- Confirm the function is linked into the `cdylib` and attributed with `#[excel_function]`.
- Check whether it is `hidden`.
- Look for a registration-name conflict with another XLL.
- Keep the UDF `id` unique within the crate.
- Run `cargo xlfn check`; it compares `.xllexp` entries with actual PE exports.
- Restart Excel after replacing an XLL. Excel may still hold the old module.

A registration conflict is rejected; xlfn does not overwrite another add-in's name.

## A cell shows `#VALUE!`

Typical causes:

- strict type mismatch, such as text supplied to `f64`;
- a blank or missing policy rejected the argument;
- invalid UTF-16 or malformed array/reference structure;
- a failed Excel callback or coercion;
- an internal or application-adapter error.

Check the argument named in diagnostics. xlfn does not perform broad Excel coercion for ordinary parameters.

## A cell shows `#NUM!`

Typical causes:

- non-finite input or result;
- `i64` outside Excel's exact `-2^53..=2^53` range;
- numeric conversion overflow;
- a domain error such as an invalid model state.

Validate model outputs before returning them. NaN and infinity are rejected rather than written into an XLOPER12.

## A cell shows `#N/A`

Typical causes:

- invalid, stale, wrong-type, or previous-session handle;
- add-in or worker is closing;
- overloaded/reentrant operation;
- an intentionally unavailable result;
- an input-only `ExcelValue::Missing` or blank `ExcelCellValue` being treated as a worksheet return. Use `ExcelError::NotAvailable` for an explicit `#N/A` result.

Recalculate the handle-producing formula first. Do not edit or persist token text as an application identifier.

## A handle does not appear to refresh

The visible token remains stable for the same formula revision by design. A
same-revision recalculation reuses the memoized object without invoking the
producer again. Changing an explicit revision input creates a new object and
token; a live token never changes the object it identifies.

Test the object's behavior or expose a safe version field rather than using token-string changes as evidence of refresh. Verify that the producer is actually recalculated and is not blocked by Excel calculation settings.

## An async formula never completes

- Confirm the crate enabled the `async` feature.
- Verify the linked async exports with `cargo xlfn check`.
- Ensure blocking work is submitted to a dedicated worker rather than occupying all async executor threads.
- Inspect the `RuntimeConfig` async worker count and downstream queue capacity.
- Check cancellation; a cancelled call deliberately suppresses late delivery.
- Ensure the future retains every needed owned input and does not wait on a resource that requires the Excel thread.
- Verify that an external client actually wakes the future.

A cancellation token cannot interrupt a blocking foreign call. Instrument queue wait and adapter execution separately.

## RTD does not update

- The worksheet function must subscribe from `MainThreadContext`.
- `RtdSource::subscribe` must return without unbounded blocking.
- Keep the returned subscription alive and keep its producer active.
- Handle errors from `RtdSink::publish`.
- Publish only supported scalar, finite, bounded values.
- Confirm Excel calculation is enabled.
- Test one, two, and three-topic batches; do not rely on a single happy path.
- Check temporary COM registration access and stale-registration recovery.
- Verify `request_cancel` does not block and `disconnect_and_wait` reaches quiescence on success, error, and unwinding.

A tight retry loop after a permanent publish failure can create an error storm and fill diagnostics.

## External adapter fails to initialize

Typical diagnostics depend on the application adapter and may include configuration failure, path-resolution failure, missing sidecar, protocol mismatch, authentication failure, unavailable service, missing symbol, ABI mismatch, or wrong architecture.

Check:

1. the declared DLL basename exactly matches the packaged file;
2. x86 and x64 metadata point to the correct files;
3. the DLL and all non-system dependencies are in the package;
4. required symbols match spelling and decoration;
5. any application-defined protocol or ABI negotiation returns the expected version;
6. antivirus or policy did not block the DLL;
7. the final installation directory has not been modified.

For packaged PE components, use an external PE inspection tool and `cargo xlfn check`. For other adapters, use the diagnostics and qualification tools appropriate to the chosen transport. Do not weaken a required contract merely to bypass initialization failure.

## External calls serialize unexpectedly

Serialization is an application-adapter policy, not an xlfn runtime policy. Inspect the adapter's locks, queue topology, per-session affinity, downstream rate limits, and external implementation contract. Multiple Excel MTR calls or multiple application workers do not imply downstream concurrency. Enable concurrent dispatch only when the complete application contract covers calls, contexts, object operations, callbacks, and destruction, then measure actual throughput.

## Excel hangs during close

A safe XLL close waits for in-process work to become quiescent. A hang usually indicates:

- running external or application code cannot be cancelled or bounded;
- an RTD subscription did not honor `request_cancel`;
- `disconnect_and_wait` waits for a callback that needs a held lock;
- application-owned background work was not joined;
- a destructor performs blocking or reentrant work;
- graceful worker shutdown is draining an unexpectedly large queue.

Do not add a timeout that lets Excel unload while code may still execute. Capture thread dumps, identify the owner and wait dependency, then fix cancellation or move the uninterruptible operation out of process.

## `cargo xlfn package` refuses paths or imports

- `artifact-name` must be a valid non-reserved Windows filename.
- Configured companion/sidecar paths are relative to the package manifest directory.
- Bundled basenames must be unique case-insensitively and must not collide with the XLL or `build-manifest.json`.
- Non-system DLL dependencies must be located and packaged alongside the add-in.
- Ensure the build output directory is writable and not locked by another process.

## Reporting an issue

When filing a bug or asking for help, include:
- Windows version and Excel version (including bitness: 32-bit or 64-bit);
- Rust toolchain and target triple used to build;
- Exact command run and complete terminal output;
- If relevant, the formula called and any diagnostic error output from the logs.
