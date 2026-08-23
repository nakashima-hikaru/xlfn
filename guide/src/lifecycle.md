# Add-in lifecycle and state

The `Addin` trait defines one open generation of the XLL:

```rust
pub trait Addin: Send + Sync + 'static {
    type SharedState: Send + Sync + 'static;
    type LifecycleState: 'static;
    type Error: IntoXllError;
    type Layers: UdfLayers;

    fn open(
        context: &OpenContext,
    ) -> Result<Opened<Self::SharedState, Self::LifecycleState, Self::Layers>, Self::Error>;
    fn quiesce(
        shared: &mut Self::SharedState,
        lifecycle: &mut Self::LifecycleState,
    ) -> Result<(), Self::Error>;
    fn cleanup(lifecycle: &mut Self::LifecycleState, reporter: &mut CleanupReporter<'_>);
}
```

`Opened` returns shared state, lifecycle-local state, execution layers, and the
runtime policy as one open transaction. `SharedState` is borrowed by UDF calls
and must be `Send + Sync`; `LifecycleState` is retained by xlfn in thread-local
storage and bound to the Excel lifecycle thread for the open generation, so it
may own thread-affine resources. `RuntimeConfig` can
select RTD limits and, with the `async` feature, the async worker count.

The stable default uses `type Layers = ();`. Custom UDF layers are part of the
stable execution contract and are documented separately in [UDF execution
layers](udf-layers.md). Other lower-level APIs remain behind the explicit
`unstable-cache` and `unstable-output` are independent experimental API
features; internal lifecycle refinement is enabled only by `refinement` and
is not required by add-in authors.

## Open

`Addin::open` runs on Excel's main lifecycle thread. `OpenContext` provides:

- `module_path()` — the full path reported for the loaded XLL;
- `module_directory()` — the directory containing the XLL;
- `build_info()` — add-in ID, crate version, and target triple.
- `rtd().register_source(...)` — an opaque RTD source identity for later subscriptions.

Use this hook to load bounded configuration, install diagnostics, and create application-owned resources needed by later calls. Return an error rather than panicking. The framework converts the error through `IntoXllError`, records diagnostics, and fails the open operation safely.

Do not perform unbounded I/O or long-running initialization in `open`. Excel is waiting synchronously.

## Shared state

Synchronous contexts borrow `&SharedState`. The framework-owned asynchronous
future keeps the current open generation (`GenerationLease`) alive, while
`AsyncContext<'_, A>` borrows the shared generation state and per-call
cancellation token for the invocation. Shared state therefore needs explicit
synchronization for mutable data:

```rust
use std::sync::RwLock;

pub struct SharedState {
    settings: RwLock<Settings>,
}
```

Prefer immutable snapshots or narrowly scoped locks. Never hold an application lock while calling Excel, invoking a user-supplied callback, waiting for a worker, or shutting down another subsystem.

## UDF layers

`Opened` returns process-local execution middleware together with state. Layers
are installed for the open generation and receive call metadata before
argument conversion. See [UDF execution layers](udf-layers.md).

## Async worker count

With the `async` feature:

```rust
impl Addin for ServiceAddin {
    type SharedState = State;
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(_: &OpenContext) -> XllResult<Opened<Self::SharedState, Self::LifecycleState, Self::Layers>> {
        Ok(Opened::new(State::new(), (), ()).with_runtime_config(
            RuntimeConfig::new().with_async_worker_count(
                AsyncWorkerCount::new(4).expect("4 is within the supported range"),
            ),
        ))
    }
}
```

`AsyncWorkerCount` accepts only values in `1..=32`; values outside that range
are rejected, and the default is four. This pool executes Rust futures and is
separate from any executor, worker, connection pool, or other runtime created
by the application.

## Quiescence, cleanup, and unload safety

`Addin::quiesce` runs on the same main lifecycle thread as `open`, after the framework has stopped accepting new calls and drained active framework calls and asynchronous tasks. It must synchronously stop every application-owned thread, callback, task, queue, and producer that could execute XLL code or require add-in state after unload.

The formula-handle registry is closed after `quiesce` returns. Formula-owned Rust objects can therefore still exist while application quiescence is being established. If a handle object refers to an application-owned resource, `quiesce` must leave its later `Drop` safe after workers, connections, or owner threads have stopped. Prefer releasing or invalidating such resources while their owners are still available, then make the later wrapper drop local or idempotent. See [Formula-owned handles](handles.md).

```rust
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

Excel's `xlAutoClose` export is an ambiguous deactivation or shutdown hint. It
does not tear down the runtime and does not release the DLL's physical
residency lease. UDFs remain callable after the hint while the generation is
still `Open`.

`xlAutoRemove` is the explicit terminal-removal boundary. It is the only
boundary that runs `quiesce`, unregisters Excel callbacks, stops framework
producers, closes RTD/COM state, reclaims the generation, and publishes the
logical `Closed` phase. After a successful removal, the following
`xlAutoClose` releases the module residency lease. `DllCanUnloadNow` remains
`S_FALSE` while that lease is held.

If `quiesce` fails or panics, or any other teardown hazard prevents a complete
certificate, the runtime enters `Quarantined`. It rejects new UDF calls and
opens, retains the module residency lease, and retains resources whose
destruction was not proven safe. Ordinary `xlAutoClose` hints never clear this
state.

If Excel requests `xlAutoOpen` while a generation is still open, xlfn performs
a controlled terminal teardown of the old generation and then opens a new
generation. A failed reload is quarantined. Normal Excel process termination
does not provide the same `quiesce` guarantee; process exit is therefore not
used as the logical lifecycle boundary.

For application-owned concurrent or thread-affine resources, a safe shutdown sequence is:

1. reject new application submissions;
2. signal cancellation or shutdown;
3. resolve or reject queued requests according to the application's contract;
4. release thread-affine resources on the thread that owns them;
5. join every application-owned worker or coordinator;
6. release remaining application roots before `quiesce` returns.

xlfn cannot prove those application-level properties; `quiesce` is the boundary at which the add-in must establish them.

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
`cleanup` must not start work or register callbacks. Only a completed cleanup,
an explicit lifecycle-state take and drop, and an empty thread-affine binding
permit the runtime to publish `Closed` and release its lifecycle-thread
binding.

Consequences:

- cancellation must be cooperative;
- background callbacks must be quiescent before `quiesce` returns;
- every application-owned worker or coordinator must be joined;
- in-process work that cannot be interrupted should be isolated out of process when safe unload requires a hard stop;
- do not implement a timeout that abandons in-process code and then permits unload.

## Application-owned lifecycle resources

`Addin::open`, `Addin::quiesce`, and `Addin::cleanup` for a generation run on
the same lifecycle thread. xlfn records that affinity as an internal
capability; a wrong-thread open or removal boundary is rejected and
quarantined before lifecycle state is accessed. An application may use this property for
lifecycle-owned registries or other resources that are not exposed to
worksheet calls. `SharedState` remains `Send + Sync + 'static`; expose only
safe, thread-compatible clients through it. `LifecycleState` is the place for
thread-affine owners.

For a thread-affine application resource, lifecycle code may own the resource
and its worker while `SharedState` exposes only a safe client. Do not let
submitted work capture and destroy the owner responsible for joining its own
worker. xlfn constrains the Excel boundary and unload ordering; the
application's internal dispatch design remains ordinary Rust code.
