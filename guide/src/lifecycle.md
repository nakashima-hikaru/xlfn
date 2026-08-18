# Add-in lifecycle and state

The `Addin` trait defines one open generation of the XLL:

```rust
pub trait Addin: Send + Sync + 'static {
    type State: Send + Sync + 'static;
    type Error: IntoXllError;

    fn open(context: &OpenContext) -> Result<Self::State, Self::Error>;
    fn udf_layers(state: &Self::State) -> Vec<Box<dyn UdfLayer>>;
    fn quiesce(state: &mut Self::State) -> Result<(), Self::Error>;
    fn cleanup(state: &mut Self::State, reporter: &mut CleanupReporter<'_>);
}
```

With the `async` feature, it also exposes `async_worker_count`.

## Open

`Addin::open` runs on Excel's main lifecycle thread. `OpenContext` provides:

- `module_path()` — the full path reported for the loaded XLL;
- `module_directory()` — the directory containing the XLL;
- `build_info()` — add-in ID, crate version, and target triple.

Use this hook to load bounded configuration, install diagnostics, and create application-owned resources needed by later calls. Return an error rather than panicking. The framework converts the error through `IntoXllError`, records diagnostics, and fails the open operation safely.

Do not perform unbounded I/O or long-running initialization in `open`. Excel is waiting synchronously.

## Shared state

Contexts expose `&State`, or an `Arc<State>` for asynchronous functions. State therefore needs explicit synchronization for mutable shared data:

```rust
use std::sync::RwLock;

pub struct State {
    settings: RwLock<Settings>,
}
```

Prefer immutable snapshots or narrowly scoped locks. Never hold an application lock while calling Excel, invoking a user-supplied callback, waiting for a worker, or shutting down another subsystem.

## UDF layers

`Addin::udf_layers` returns process-local execution middleware. Layers are installed for the open generation and receive call metadata before argument conversion. See [UDF execution layers](udf-layers.md).

## Async worker count

With the `async` feature:

```rust
fn async_worker_count(_: &State) -> usize {
    4
}
```

The framework clamps the value to `1..=32`. The default is the available parallelism capped at four. This pool executes Rust futures; it is separate from any executor, worker, connection pool, or other runtime created by the application.

## Quiescence, cleanup, and unload safety

`Addin::quiesce` runs on the same main lifecycle thread as `open`, after the framework has stopped accepting new calls and drained active framework calls and asynchronous tasks. It must synchronously stop every application-owned thread, callback, task, queue, and producer that could execute XLL code or require add-in state after unload.

The formula-handle registry is closed after `quiesce` returns. Formula-owned Rust objects can therefore still exist while application quiescence is being established. If a handle object refers to an application-owned resource, `quiesce` must leave its later `Drop` safe after workers, connections, or owner threads have stopped. Prefer releasing or invalidating such resources while their owners are still available, then make the later wrapper drop local or idempotent. See [Formula-owned handles](handles.md).

```rust
fn quiesce(state: &mut State) -> Result<(), Error> {
    state.request_application_shutdown();
    state.join_application_workers()?;
    Ok(())
}
```

`xlAutoClose` cannot reject DLL unload. If `quiesce` fails or panics, the framework fail-stops because unload safety is unknown. It also fail-stops when an Excel callback remains registered, a framework producer cannot be stopped, an RTD/COM object remains live, a handle runtime is not quiescent, or an `Arc<State>` escaped.

For application-owned concurrent or thread-affine resources, a safe shutdown sequence is:

1. reject new application submissions;
2. signal cancellation or shutdown;
3. resolve or reject queued requests according to the application's contract;
4. release thread-affine resources on the thread that owns them;
5. join every application-owned worker or coordinator;
6. release remaining application roots before `quiesce` returns.

xlfn cannot prove those application-level properties; `quiesce` is the boundary at which the add-in must establish them.

After quiescence, `Addin::cleanup` performs best-effort disposal. It cannot return an arbitrary business error. Report recoverable failures explicitly; they are logged without preventing safe unload:

```rust
fn cleanup(state: &mut State, reporter: &mut CleanupReporter<'_>) {
    if let Err(error) = state.remove_cached_metadata() {
        reporter.warn("metadata cache", CleanupIssueKind::HostMetadata, error);
    }
}
```

A cleanup panic is contained after quiescence. The framework leaks the state rather than invoking more unknown destructor code, records the issue, and completes unload. `cleanup` must not start work or register callbacks.

Consequences:

- cancellation must be cooperative;
- background callbacks must be quiescent before `quiesce` returns;
- every application-owned worker or coordinator must be joined;
- in-process work that cannot be interrupted should be isolated out of process when safe unload requires a hard stop;
- do not implement a timeout that abandons in-process code and then permits unload.

## Application-owned lifecycle resources

`Addin::open`, `Addin::quiesce`, and `Addin::cleanup` for a generation run on the same lifecycle thread. An application may use this property for lifecycle-owned registries or other resources that are not exposed to worksheet calls. `State` itself remains `Send + Sync + 'static`; expose only safe, thread-compatible clients through it.

For a thread-affine application resource, lifecycle code may own the resource and its worker while `State` exposes only a safe client. Do not let submitted work capture and destroy the owner responsible for joining its own worker. xlfn constrains the Excel boundary and unload ordering; the application's internal dispatch design remains ordinary Rust code.
