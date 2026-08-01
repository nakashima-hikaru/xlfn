# Add-in lifecycle and state

The `Addin` trait defines one open generation of the XLL:

```rust
pub trait Addin: Send + Sync + 'static {
    type State: Send + Sync + 'static;
    type Error: IntoXllError;

    fn open(context: &OpenContext) -> Result<Self::State, Self::Error>;
    fn udf_layers(state: &Self::State) -> Vec<Arc<dyn UdfLayer>>;
    fn close(state: &mut Self::State) -> Result<(), Self::Error>;
}
```

With the `async` feature, it also exposes `async_worker_count`.

## Open

`Addin::open` runs on Excel's main lifecycle thread. `OpenContext` provides:

- `module_path()` — the full path reported for the loaded XLL;
- `module_directory()` — the directory containing the XLL;
- `build_info()` — add-in ID, crate version, and target triple.

Use this hook to load bounded configuration, install diagnostics, verify native libraries, and create resources needed by later calls. Return an error rather than panicking. The framework converts the error through `IntoXllError`, records diagnostics, and fails the open operation safely.

Do not perform unbounded network or native work in `open`. Excel is waiting synchronously.

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

The framework clamps the value to `1..=32`. The default is the available parallelism capped at four. This pool executes Rust futures; it is separate from any native `ThreadBoundOwner` or coordinator-backed `ThreadBoundPool` you create for native objects.

## Close and unload safety

`Addin::close` runs on the same main lifecycle thread as `open`, after the framework has stopped accepting new calls and drained active framework calls and asynchronous tasks. Use it to synchronously recover and release lifecycle-owned resources.

```rust
fn close(state: &mut State) -> Result<(), Error> {
    state.request_application_shutdown();
    shutdown_native_owners()?;
    Ok(())
}
```

`xlAutoClose` is terminal. Excel does not provide a useful retry protocol for a half-closed XLL. The framework therefore diagnoses a close error and still releases state. A close hook must not return while code from the XLL can still execute.

Consequences:

- cancellation must be cooperative;
- background callbacks must be quiescent before close returns;
- native worker owners must be joined;
- a native operation that cannot be interrupted should be isolated out of process;
- do not implement a timeout that abandons in-process code and then permits unload.

## Thread-affine lifecycle owners

`Addin::open` and `Addin::close` for a generation run on the same lifecycle thread. This permits a thread-local or otherwise external registry for non-`Send` owners. Put only their cloneable, `Send + Sync` submission handles in `State`.

This owner/handle split is intentional: a worker operation cannot capture and destroy the object responsible for joining that same worker.
