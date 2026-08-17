# Formula-owned handles

Handles let a worksheet formula own an ownership edge to a typed Rust object without exposing a pointer or serialized object graph. A producer returns the object itself; a consumer accepts a call-scoped `Handle<'_, T>`. xlfn owns the formula binding, the published object identity, and the Rust value lifetime; multiple formula bindings may refer to the same object through an explicit alias. Any resource managed inside `T` remains part of `T`'s application-level contract.

## Define a handle object

```rust
use xlfn::{error::InputError, prelude::*};

#[derive(ExcelHandleObject)]
pub struct Dataset {
    times: Vec<f64>,
    values: Vec<f64>,
}

impl Dataset {
    fn evaluate(&self, time: f64) -> XllResult<f64> {
        let first = self.times[0];
        let last = self.times[self.times.len() - 1];
        if time < first || time > last {
            return Err(XllError::input("time", InputError::OutOfRange));
        }

        let right = self.times.partition_point(|pillar| *pillar < time);
        if right == 0 {
            return Ok(self.values[0]);
        }
        if right == self.times.len() {
            return Ok(self.values[right - 1]);
        }

        let left = right - 1;
        let weight = (time - self.times[left])
            / (self.times[right] - self.times[left]);
        Ok(self.values[left]
            + weight * (self.values[right] - self.values[left]))
    }
}
```

The derived trait requires the value to be `Any + Send + Sync + 'static`. The object may contain immutable data, synchronized application clients, typed resource identifiers, or other owned Rust values. It must not contain call-scoped Excel references.

## Produce and consume

```rust
#[excel_function(name = "CURVE.CREATE")]
fn create_dataset(times: Row<f64>, values: Row<f64>) -> XllResult<Dataset> {
    let times = times.into_vec();
    let values = values.into_vec();
    if times.len() != values.len() || times.is_empty() {
        return Err(XllError::input(
            "times",
            InputError::Malformed("times and values must have equal non-zero length"),
        ));
    }
    if times[0] < 0.0 || times.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(XllError::input(
            "times",
            InputError::Malformed("times must be non-negative and strictly increasing"),
        ));
    }
    if values.iter().any(|val| *val <= 0.0) {
        return Err(XllError::input(
            "values",
            InputError::Malformed("values must be positive"),
        ));
    }
    Ok(Dataset { times, values })
}

#[excel_function(name = "DATASET.EVALUATE", thread_safe)]
fn dataset_evaluate(dataset: Handle<'_, Dataset>, time: f64) -> XllResult<f64> {
    dataset.evaluate(time)
}
```

`Handle<'call, T>` is a borrowed, call-scoped capability. It dereferences to `T`,
and its lifetime cannot outlive the active Excel call. It is neither `Clone` nor
an owned return value; do not store it in Add-in state, another handle object, or
an async task.

No `handle` argument attribute is required. Ordinary Rust trait resolution identifies `Handle<'_, T>`.

## Re-evaluation semantics

Handle-producing functions are memoized by formula revision.

The worksheet cell is the formula owner. A revision is identified by that caller, the stable producer UDF ID, and an input fingerprint. Recalculation with the same revision reuses the existing formula binding and handle object without invoking the producer again.

Changing the caller, producer ID, or input fingerprint creates a new formula binding, object, and token.

A live token never changes the object it identifies.

The input fingerprint is a runtime-local BLAKE3 fingerprint of the converted Rust arguments. It is an implementation detail for memoization, not a stable serialized or cross-version identifier. Each ordinary parameter contributes its semantic identity: for example, a `Handle<'_, T>` contributes its `ObjectId`, an enum contributes its normalized variant, and a defaulted argument contributes the value after default conversion. Raw Excel representation is retained only by explicitly raw-view or reference parameters such as `XlArrayRef`. Conversion and array layers enforce their own workbook-controlled resource bounds. Different tokens that alias the same object therefore have the same semantic input identity. The fingerprint distinguishes input revisions for memoization; it is not itself the ownership identity.

The caller portion uses Excel's stable sheet identifier. Workbook and worksheet display names are used only to resolve that identifier and are not part of the runtime key, so renaming a sheet, renaming a workbook, or using Save As does not by itself create a new formula revision.

Changing the caller, function ID, or arguments creates a different formula revision. A producer must be deterministic: its output must depend only on its Excel-visible inputs and stable application state explicitly represented by those inputs.

### External state and dependency design

Because the producer runs at most once per formula revision, reading hidden mutable state inside the producer does not produce automatic updates when that state changes. Make varying state an explicit Excel-visible dependency:

```rust
// NG: hidden mutable state is read but never triggers re-evaluation.
fn market() -> Market {
    database.load_latest()
}

// OK: changing snapshot_id changes the input fingerprint and revision, creating a new object.
fn market(snapshot_id: String) -> Market { .. }

// OK: changing the underlying upstream object changes the downstream input fingerprint;
// aliases of the same object retain the same semantic identity.
fn model(market: Handle<'_, MarketSnapshot>) -> Model { .. }
```

## Handle alias functions

A function may explicitly republish an existing handle through `HandleAlias`:

```rust
#[excel_function(name = "CURVE.ALIAS")]
fn alias(dataset: Handle<'_, Dataset>) -> HandleAlias<'_, Dataset> {
    dataset.alias()
}
```

`HandleAlias<'call, T>` is the only handle return capability. It is an
identity-only object capability: it carries the identity of the underlying
object and may retain a private lifetime pin so that object remains
publishable until the alias is consumed. It does not expose a snapshot, `Arc`,
or `&T`. Publishing it creates a new formula binding and token that shares the
same underlying `HandleObject`; it does not clone the business object. A plain
`Handle` cannot be returned, cloned, or retained after the call.

## Lifetime

Each worksheet formula owns one runtime binding edge. The shared object is
released after the last binding is removed, or when the registry closes after
the relevant workbook dependency, RTD topic, or add-in terminates.

Destructors must obey the same shutdown rules as any in-process code:

- do not call Excel;
- do not block indefinitely;
- do not panic;
- do not directly destroy a thread-affine application resource from an arbitrary handle destructor.

The runtime supports at most 16,384 live handles per open generation. This is a safety bound, not a capacity target.

## Resource-backed handle objects

A handle object may represent or refer to a resource that is owned elsewhere in the application. Prefer a safe Rust client plus a typed logical identifier over a raw pointer. If a raw pointer is unavoidable, the application must independently prove that movement, concurrent access, and destruction from every possible drop thread are valid; adding `unsafe impl Send` or `Sync` only to satisfy `ExcelHandleObject` does not establish those properties.

If explicit close and `Drop` can both release the same application resource, make release idempotent. Dependencies between application resources are also application state: do not rely on an incidental Rust drop order when explicit invalidation can make a still-referenced object unusable.

## Shutdown interaction

The close order relevant to handle objects is:

1. xlfn stops and drains framework-managed work;
2. `Addin::quiesce` establishes application-level quiescence;
3. xlfn closes the formula-handle registry and drops remaining Rust handle objects;
4. `Addin::cleanup` performs bounded best-effort disposal.

A handle object's `Drop` therefore must remain safe after `quiesce` has stopped application workers or owner threads. If resource destruction requires such an owner, release or invalidate the resource during `quiesce` while the owner is still available, and make the later Rust wrapper drop a local or idempotent operation. Do not defer the only copy of an application shutdown protocol to handle `Drop`.

## Valid producer contexts

A newly constructed handle object uses main-thread return semantics. Producers cannot be:

- thread-safe UDFs;
- macro-sheet UDFs;
- asynchronous UDFs;
- functions with raw reference arguments;
- volatile UDFs.

`HandleAlias<'_, T>` uses main-thread return semantics. A borrowed
`Handle<'_, T>` is an input capability only and is not a valid return type.

## Caller restrictions

Formula ownership requires one worksheet-cell caller. Contexts without a stable single cell, such as direct VBA invocation, Function Wizard evaluation, or some multi-cell caller shapes, return a controlled error rather than creating an unowned object.

Document this behavior for users who expose handle producers in automation-heavy workbooks.

## Token security model

A token contains runtime/session identity, slot/generation data, and a keyed
BLAKE3 MAC. Rust type identity is intentionally not part of the wire format:
after authentication and slot/generation validation, the registry checks the
requested `T` against the canonical `BindingRecord`. Tokens from another
process generation, tokens of the wrong type, stale slot generations, and
modified tokens are rejected.

The token is a bearer capability inside the Excel process. It is not an authorization system, workbook ACL, encryption scheme, or durable serialization format. Do not parse it, persist it as an application identifier, or accept it outside the add-in's worksheet boundary.

## Object design guidance

A good handle object is:

- immutable or internally synchronized;
- cheap to share through `Arc`;
- explicit about any application-level thread affinity;
- free of workbook-owned pointers;
- bounded in memory;
- safe to drop during orderly add-in close.
