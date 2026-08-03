# External engine integration

`xlfn` owns the Excel-facing boundary of an XLL. It generates and manages Excel ABI exports, registration, value conversion, return-value ownership, formula handles, async completion, RTD transport, diagnostics, and add-in lifecycle coordination.

It does **not** provide an external-engine adapter. In particular, xlfn does not load libraries, generate bindings, select an ABI or transport, interpret external status codes, create engine worker pools, or manage external object registries. Those components belong to the add-in application.

The intended layering is:

```text
Excel
  ↓
xlfn-generated Excel boundary
  ↓
add-in functions and domain types
  ↓
application-defined adapter
  ↓
external implementation chosen by the application
```

The last boundary may be a direct Rust dependency, a statically linked component, a dynamically loaded library, COM, IPC, a local process, a remote service, or another application-specific mechanism. xlfn neither prescribes nor recommends one of these forms.

## Responsibility split

| Concern | Owner |
|---|---|
| Excel registration and exported XLL ABI | xlfn |
| `XLOPER12` conversion and return ownership | xlfn |
| formula-owned handle tokens | xlfn |
| async-result delivery and RTD transport | xlfn |
| add-in open, quiesce, cleanup, and unload barriers | xlfn |
| external bindings, loading, and protocol negotiation | application adapter |
| external error interpretation and domain mapping | application adapter |
| concurrency, thread affinity, queues, and cancellation | application adapter |
| external object identity and destruction | application adapter |
| trust policy for external code or services | application and deployment |

The adapter should expose ordinary safe Rust types to worksheet functions. Its public surface should describe domain operations rather than the mechanics of the chosen external boundary.

```rust,ignore
pub trait PricingEngine: Send + Sync {
    fn price(&self, request: PriceRequest) -> Result<f64, EngineError>;
    fn begin_shutdown(&self);
}

pub struct State {
    pub engine: std::sync::Arc<dyn PricingEngine>,
}
```

`PricingEngine`, `PriceRequest`, and `EngineError` above are application types, not xlfn APIs.

## Lifecycle integration

Construct the application adapter in `Addin::open`. `OpenContext::module_directory()` is available when the adapter needs a path relative to the installed XLL, but xlfn does not interpret or load that path.

```rust,ignore
fn open(context: &OpenContext) -> XllResult<State> {
    let engine = application_adapter::open(context.module_directory())?;
    Ok(State { engine })
}
```

During `Addin::quiesce`, stop admission, cancel or drain application work according to the adapter contract, release external objects, and join every application-owned thread that could execute XLL code after return. `Addin::cleanup` is only for bounded best-effort disposal after quiescence has already been established.

Map adapter errors to `XllError` or implement `IntoXllError` for an application error type. Do not expose raw external error buffers, pointers, or transport-specific values directly to worksheet functions.

## Packaging is not runtime integration

`[package.metadata.xlfn.bundle]` can stage sidecar files and `cargo xlfn` can inspect PE architecture and packaged DLL import closure. This is a build and distribution facility only. It does not:

- load a sidecar at runtime;
- resolve application symbols or methods;
- validate an application protocol or object model;
- prove that calls are thread-safe or cancellable;
- select how the adapter locates or authenticates an external component.

If the application does not use sidecar files, no bundle metadata is required. If it does, keep runtime loading policy inside the application adapter and deployment policy.

Continue with [Thread-affine application adapters](native-threading.md) when the chosen implementation has thread-affine state, and [External objects as formula handles](native-objects.md) when worksheet formulas own application objects.
