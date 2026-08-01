# Troubleshooting

Start with the earliest failing boundary. Do not debug a worksheet result before confirming that the correct XLL loaded and its dependencies resolved.

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
- an internal or native adapter error.

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
- `OwnedExcelValue::Blank` or `Missing` used as a worksheet return.

Recalculate the handle-producing formula first. Do not edit or persist token text as an application identifier.

## A handle does not appear to refresh

The visible token remains stable for the same formula identity by design. The producer body still runs on every Excel evaluation, and the underlying object is replaced after successful observation.

Test the object's behavior or expose a safe version field rather than using token-string changes as evidence of refresh. Verify that the producer is actually recalculated and is not blocked by Excel calculation settings.

## An async formula never completes

- Confirm the crate enabled the `async` feature.
- Verify the linked async exports with `cargo xlfn check`.
- Ensure blocking work is submitted to a dedicated worker rather than occupying all async executor threads.
- Inspect `async_worker_count` and downstream queue capacity.
- Check cancellation; a cancelled call deliberately suppresses late delivery.
- Ensure the future retains every needed owned input and does not wait on a resource that requires the Excel thread.
- Verify that an external client actually wakes the future.

A cancellation token cannot interrupt a blocking foreign call. Instrument queue wait and native execution separately.

## RTD does not update

- The worksheet function must subscribe from `MainThreadContext`.
- `RtdSource::subscribe` must return without unbounded blocking.
- Keep the returned subscription alive and keep its producer active.
- Handle errors from `RtdSink::publish`.
- Publish only supported scalar, finite, bounded values.
- Confirm Excel calculation is enabled.
- Test one, two, and three-topic batches; do not rely on a single happy path.
- Check temporary COM registration access and stale-registration recovery.
- Verify `request_cancel` does not block and `disconnect_and_wait` reaches quiescence.

A tight retry loop after a permanent publish failure can create an error storm and fill diagnostics.

## Native DLL fails to load

Typical diagnostics include path-resolution failure, unsupported platform, missing symbol, symbol lookup error, ABI mismatch, or wrong architecture.

Check:

1. the declared DLL basename exactly matches the packaged file;
2. x86 and x64 metadata point to the correct files;
3. the DLL and all non-system dependencies are in the package;
4. required symbols match spelling and decoration;
5. the ABI version function returns the expected value;
6. antivirus or policy did not block the DLL;
7. the final installation directory has not been modified.

Use an external PE inspection tool and `cargo xlfn check`; do not "fix" a missing required symbol by making it optional.

## Native calls serialize unexpectedly

`SerializedLibrary<A>` serializes calls per canonical DLL path. A worker pool does not bypass that
gate.

Only call `VerifiedLibrary::assume_concurrent` when the native contract covers concurrent
functions, contexts, object operations, callbacks, and destruction. Then measure whether Excel
MTR, worker selection, and backend capacity actually produce concurrency.

## Excel hangs during close

A safe XLL close waits for in-process work to become quiescent. A hang usually indicates:

- running native code cannot be cancelled or bounded;
- an RTD subscription did not honor `request_cancel`;
- `disconnect_and_wait` waits for a callback that needs a held lock;
- application-owned background work was not joined;
- a destructor performs blocking or reentrant work;
- graceful worker shutdown is draining an unexpectedly large queue.

Do not add a timeout that lets Excel unload while code may still execute. Capture thread dumps, identify the owner and wait dependency, then fix cancellation or move the uninterruptible operation out of process.

## `cargo xlfn dist` refuses paths or imports

- `artifact-name` must be a valid non-reserved Windows basename.
- Native metadata paths are relative to the package manifest directory.
- Native basenames must be unique case-insensitively and must not collide with the XLL or `build-manifest.json`.
- With `strict-paths = true`, configured paths cannot traverse symlinks or reparse points.
- Every non-system import must be packaged or explicitly approved as an external import.
- `dist --all` requires a dedicated replaceable output directory.

When commit and rollback both fail, preserve and inspect the recovery path reported by the tool.

## Escalating an issue

A useful OSS issue contains a minimal reproducer, exact command output, environment matrix, diagnostic IDs, and a statement of whether the failure occurs in Rust tests, package validation, or real Excel. Remove proprietary workbooks and native binaries unless redistribution is authorized; replace them with a minimal mock when possible.
