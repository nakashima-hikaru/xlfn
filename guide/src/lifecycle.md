# Add-in lifecycle and state

Use `Addin` to create resources once when Excel opens the add-in and share
those resources between worksheet calls. No optional feature is required.
This chapter builds on the project from [Create your first add-in](quick-start.md).
For choosing handles, caches, or background work, see
[Choosing a pattern](choosing-pattern.md).

## Create shared state

Replace the tutorial's add-in definition and `Addin` implementation with this
example. It keeps its shared state immutable:

```rust
use xlfn::prelude::*;

#[excel_addin(name = "Service Add-in", id = "service-addin", category = "Service")]
pub struct ServiceAddin;

pub struct State {
    scale: f64,
}

impl Addin for ServiceAddin {
    type SharedState = State;
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(_: &OpenContext) -> OpenResult<Self> {
        Ok(Opened::new(State { scale: 2.0 }))
    }
}

#[excel_function(name = "SERVICE.SCALE")]
fn scale(
    context: ThreadSafeContext<'_, ServiceAddin>,
    value: f64,
) -> f64 {
    value * context.state().scale
}
# fn main() {}
```

Use the `Addin` implementation on the type declared by your project's
`#[excel_addin]` attribute. Each successful open creates a fresh state instance;
worksheet calls borrow that instance until the generation is closed.

`OpenResult<Self>` uses the state, layers, and error types declared by this
implementation. `type Error` implements `IntoXllError`; `XllError` is a convenient starting
point. Return errors from initialization rather than panicking. The framework
records a failed open and prevents worksheet calls from using partial state.
An error or panic during `Addin::open` prevents the add-in from completing registration.

## Choose where each resource belongs

| Place | Use for | Required lifetime and access |
| --- | --- | --- |
| `SharedState` | Immutable data, synchronized clients, calculation caches | `Send + Sync + 'static`; borrowed by worksheet calls |
| `LifecycleState` | Owners tied to the Excel lifecycle thread | `'static`; accessed by `open`, `quiesce`, and `cleanup` on that thread |
| `Layers` | UDF admission policy and instrumentation | `()` or a tuple of one to sixteen `UdfLayer` values |

`Opened::new(state)` starts with `LifecycleState = ()`, `Layers = ()`, and
default runtime settings. Add the pieces that your application needs:

```rust
# use xlfn::prelude::*;
# let (shared_state, lifecycle_state, layers) = ((), (), ());
# let runtime_config = xlfn::RuntimeConfig::new();
let opened = Opened::new(shared_state)
    .with_lifecycle(lifecycle_state)
    .with_layers(layers)
    .with_runtime_config(runtime_config);
```

A thread-affine resource belongs in `LifecycleState`. Expose a safe,
thread-compatible client through `SharedState` when worksheet calls need it.
Do not let submitted work own the object responsible for joining its own worker.

## Load configuration and locate installed files

`Addin::open` runs synchronously on Excel's main lifecycle thread.
`OpenContext` provides:

- `module_path()` — the loaded XLL's full path;
- `module_directory()` — its installation directory;
- `build_info()` — add-in ID, crate version, and target triple;
- `diagnostics()` — installation of a diagnostic sink;
- `rtd().register_source(...)` — source registration when `rtd` is enabled.

Use `open` to read bounded configuration, install diagnostics, and construct
application resources. Avoid unbounded I/O and long-running initialization:
Excel waits for this function to finish. Locate installed sidecars from the
module directory; do not rely on Excel's current working directory.

## Share mutable data carefully

Synchronous and asynchronous contexts borrow shared state for their invocation.
An async invocation keeps its state alive until that task ends; its context
cannot be moved into a detached task. See [Asynchronous functions](async-functions.md).

Prefer immutable snapshots. Where mutation is necessary, use a narrowly scoped
lock or another synchronization mechanism appropriate to the application:

```rust
use std::sync::RwLock;
# struct Settings;

struct SharedState {
    settings: RwLock<Settings>,
}
```

Never hold an application lock while calling Excel, invoking a user callback,
waiting for a worker, or shutting down another subsystem. `Send + Sync` alone
does not establish those application-level ordering rules.

## Configure optional services

Pass a `RuntimeConfig` to `Opened::with_runtime_config` during `open`.
Each service has one configuration entry point:

| Feature | Runtime builder | Details |
| --- | --- | --- |
| `handles` | `with_handles(HandleConfig)` | [Handle capacity and lifetime](handles.md#lifetime) |
| `async` | `with_async(AsyncConfig)` | [Async worker count](async-functions.md#executor-capacity) |
| `rtd` | `with_rtd(RtdConfig)` | [RTD limits](rtd.md#topic-and-capacity-limits) |

Services not configured explicitly use their defaults. Application-created
executors, connection pools, and queues remain the application's responsibility.
Calculation caches are configured separately; see [Calculation caches](caching.md).

Use `type Layers = ();` when no execution layers are needed. Custom layers
receive call metadata and completion status for admission decisions, metrics,
and tracing. Returning an error from `enter` rejects the invocation before the
UDF runs. Layers cannot inspect or change arguments or return values, or replace
the UDF implementation.

## Plan shutdown alongside initialization

For every resource created in `open`, decide how it stops. The key hooks are:

- `quiesce`: stop new application work, cancel background workers, and prepare
  for safe object destruction;
- `cleanup`: perform best-effort cleanup after quiescence and report any
  recoverable issues through `CleanupReporter`.

Framework-managed async tasks drain before `quiesce`; remaining handle objects
are dropped after it. Destructors should avoid blocking or calling into Excel.

If shutdown cannot establish safety, the framework quarantines the runtime.
It rejects new worksheet work and reopening, and retains the resources needed
by outstanding work and any held DLL residency lease. A close or remove
callback return does not permit unloading a quarantined add-in; restart Excel
to recover. Failure to prepare the Windows COM wait for outstanding calls also
enters quarantine, without retrying that wait from cleanup or destructors.
