# Shutdown and unload

Use this chapter when your add-in owns workers, callbacks, connections, or
thread-affine resources. Start with [Add-in lifecycle and state](../../guide/src/lifecycle.md)
for construction and state access. Shutdown is part of the application
contract: dropping an XLL is not a way to stop code that is still running.

## Stop application work with quiesce

`Addin::quiesce` runs on the same main lifecycle thread as `open`, after the framework has stopped accepting new calls and drained active framework calls and asynchronous tasks. It must synchronously stop every application-owned thread, callback, task, queue, and producer that could execute XLL code or require add-in state after logical teardown. An add-in that opts into physical DLL unload must additionally implement the unsafe `PhysicallyUnloadableAddin` contract and stop every executable source before the stronger hook returns.

The formula-handle registry is closed after `quiesce` returns. Formula-owned Rust objects can therefore still exist while application quiescence is being established. If a handle object refers to an application-owned resource, `quiesce` must leave its later `Drop` safe after workers, connections, or owner threads have stopped. Prefer releasing or invalidating such resources while their owners are still available, then make the later wrapper drop local or idempotent. See [Formula-owned handles](../../guide/src/handles.md).

```rust,ignore
fn quiesce(
    shared: &mut SharedState,
    lifecycle: &mut LifecycleState,
) -> Result<(), Error> {
    shared.request_application_shutdown();
    shared.join_application_workers()?;
    lifecycle.release_thread_affine_resources();
    Ok(())
}
```

## What Excel close and removal mean

Excel's `xlAutoClose` export is an ambiguous deactivation or shutdown hint. It
does not tear down the runtime and does not release the DLL's physical
residency lease. UDFs remain callable after the hint while the generation is
still `Open`.

`xlAutoRemove` is the explicit terminal-removal boundary. It is the only
boundary that runs `quiesce`, unregisters Excel callbacks, stops framework
producers, closes RTD/COM state, reclaims the generation, and publishes the
logical `Closed` phase. A normal safe `Addin` retains the module residency
lease after this transition, so the framework does not claim that arbitrary
application-created executable sources have stopped. Only an add-in using the
`physical_unload` attribute option and the unsafe
`PhysicallyUnloadableAddin` contract permits the following `xlAutoClose` to
release that lease. `DllCanUnloadNow` remains `S_FALSE` while the lease is
held.

## Failure and quarantine

If `quiesce` fails or panics, or the framework cannot establish that teardown is complete, the runtime enters `Quarantined`. It rejects new UDF calls and
opens, retains the module residency lease, and retains resources whose
destruction was not proven safe. Ordinary `xlAutoClose` hints never clear this
state.

An error or panic from `Addin::open` before it returns `Opened` also
quarantines the runtime, even when physical unload is enabled. Without the
application state, xlfn cannot call `quiesce` to stop any execution sources
started during initialization. The DLL stays resident and further opens are
rejected. Failures before `Addin::open` starts can still roll back normally;
failures after it returns `Opened` use that state to run quiescence and cleanup.

If Excel requests `xlAutoOpen` while a generation is still open, xlfn performs
a controlled terminal teardown of the old generation and then opens a new
generation. A failed reload is quarantined. Normal Excel process termination
does not provide the same `quiesce` guarantee; process exit is therefore not
used as the logical lifecycle boundary.

## Order application shutdown

For application-owned concurrent or thread-affine resources, a safe shutdown sequence is:

1. reject new application submissions;
2. signal cancellation or shutdown;
3. resolve or reject queued requests according to the application's contract;
4. release thread-affine resources on the thread that owns them;
5. join every application-owned worker or coordinator;
6. release remaining application roots before `quiesce` returns.

xlfn cannot prove those application-level properties; `quiesce` is the boundary at which the add-in must establish them.

## Report best-effort cleanup failures

After quiescence, `Addin::cleanup` performs best-effort disposal of
`LifecycleState`. It cannot return an arbitrary business error. Report
recoverable failures explicitly; they are logged without preventing safe
unload:

```rust
fn cleanup(lifecycle: &mut LifecycleState, reporter: &mut CleanupReporter<'_>) {
    if let Err(error) = lifecycle.remove_cached_metadata() {
        reporter.warn("metadata cache", CleanupIssueKind::HostMetadata, error);
    }
}
```

A cleanup panic is contained after quiescence. The framework retains or leaks
the lifecycle state rather than invoking more unknown destructor code, records
the issue, and quarantines the runtime with its module residency lease held.
`cleanup` must not start work or register callbacks. The runtime reaches `Closed` only after cleanup and destruction of lifecycle
state have completed. It then releases its association with the lifecycle
thread.

## Rules for background work

- cancellation must be cooperative;
- background callbacks must be quiescent before `quiesce` returns;
- every application-owned worker or coordinator must be joined;
- in-process work that cannot be interrupted should be isolated out of process when safe unload requires a hard stop;
- do not implement a timeout that abandons in-process code and then permits unload.

## Preserve thread ownership

`open`, `quiesce`, and `cleanup` for an open generation run on the same
lifecycle thread. A wrong-thread open or removal request is rejected and
quarantined before lifecycle state is accessed. Keep thread-affine owners in
`LifecycleState`, with only safe, thread-compatible clients in `SharedState`.
Submitted work must not capture and destroy the owner responsible for joining
its own worker.

Before distribution, test failure, reload, cancellation, and shutdown using
the [Excel testing checklist](../EXCEL_TESTING.md).
