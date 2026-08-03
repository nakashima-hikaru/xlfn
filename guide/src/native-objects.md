# External objects as formula handles

A worksheet formula can own an application object through xlfn's `Handle<T>`. xlfn manages the authenticated formula token and the lifetime of the Rust value `T`; it does not create, validate, or destroy the corresponding object in an external engine.

## Store a safe adapter reference and logical identity

For an external object, the formula-owned Rust type should normally contain:

- a cloneable, `Send + Sync` application-adapter reference;
- a typed logical object identifier;
- any application identity needed to reject cross-engine or cross-session use;
- synchronized release state when explicit close and `Drop` can race.

```rust,ignore
use std::sync::Arc;
use xlfn::prelude::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct CurveId(u64);

#[derive(ExcelHandleObject)]
pub struct Curve {
    adapter: Arc<EngineAdapter>,
    id: CurveId,
}
```

`EngineAdapter` and `CurveId` are application types. The adapter may represent a direct Rust engine, a library, a process, a service, or another implementation. xlfn has no knowledge of that choice.

Avoid storing a raw external pointer in a formula-owned object unless the application has independently proved that movement, concurrent access, and destruction from every possible drop thread are valid. Adding unsafe `Send` or `Sync` implementations merely to satisfy `ExcelHandleObject` hides rather than enforces the external contract.

## Create and consume

A handle producer returns the application object itself. A consumer receives `Handle<T>` and calls the application's safe adapter API.

```rust,ignore
#[excel_function(name = "CURVE.CREATE")]
fn curve_create(
    #[excel_context(main_thread)] context: MainThreadContext<'_, '_, State>,
    quotes: Row<f64>,
) -> XllResult<Curve> {
    context.state().engine.create_curve(quotes.into_vec())
}

#[excel_function(name = "CURVE.DISCOUNT", thread_safe)]
fn curve_discount(curve: Handle<Curve>, time: f64) -> XllResult<f64> {
    curve.adapter.discount(curve.id, time)
}
```

The producer is a main-thread function because it returns a newly constructed handle object. The adapter may perform its own work elsewhere, provided the producer returns only after it has a valid Rust object to publish.

The consumer must validate application-level identity before dispatch. For example, reject an object identifier from another engine instance or session rather than relying only on its numeric value.

## Destruction belongs to the adapter

When the last Rust reference is dropped, xlfn only drops `Curve`. The application adapter must decide how the external object is released.

During ordinary operation, `Drop` may enqueue a non-blocking release request to the owning adapter rather than call a thread-affine external destructor directly. Important constraints are:

- explicit close and `Drop` cannot destroy the same object twice;
- a failed or rejected release remains visible to final adapter shutdown;
- release submission never blocks indefinitely;
- cleanup failures are diagnosed because `Drop` cannot return them to Excel;
- object destruction never calls Excel.

At XLL close, `Addin::quiesce` runs before xlfn closes the formula-handle registry. Formula-owned Rust wrappers can therefore still exist while the application must stop its workers. The adapter must maintain an independent registry of live external objects, destroy or invalidate all of them during `quiesce`, and make later wrapper drops idempotent no-ops or local bookkeeping only. A handle `Drop` must not require a worker that `quiesce` has already joined.

If release can fail and callers need to observe the result, provide an explicit application operation in addition to `Drop`. The automatic drop path is a normal-runtime convenience and final local safety net, not the sole shutdown protocol.

## Dependencies between external objects

When one external object depends on another, record that relationship in the application adapter or external engine. Keeping only a Rust `Arc` to another formula handle is insufficient when explicit external close can invalidate the shared object while Rust references remain.

Use typed identifiers and an application-owned dependency graph or ownership tree. Define deterministic shutdown ordering and reject cross-session dependency edges.

## Re-evaluation and replacement

A handle-producing formula executes on every Excel evaluation. For the same formula identity, xlfn preserves the visible token and replaces the Rust object after successful observation. The previous Rust object is then dropped.

Consequently, a producer may create and release an external object on each recalculation. When that is too expensive, use an application cache keyed by immutable inputs or change the workbook calculation design. Do not expect the handle runtime to skip the producer body.

## Shutdown ordering

Do not rely on an incidental drop order between formula objects and application state. The implemented close order is relevant: xlfn calls `Addin::quiesce`, then closes the formula-handle registry, then calls `Addin::cleanup`. The adapter should therefore use this sequence:

1. stop new application submissions and mark the adapter closing;
2. destroy or invalidate every registered external object in dependency order while owner threads are still available;
3. join owner threads or close external sessions before `quiesce` returns;
4. let the subsequent xlfn handle-registry close drop Rust wrappers without performing external work;
5. use `Addin::cleanup` only for bounded best-effort disposal of already-quiescent state.

xlfn enforces its own handle-runtime quiescence, but external object quiescence is the application's responsibility and must be completed by `Addin::quiesce`.
