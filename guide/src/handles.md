# Formula-owned handles

Handles let a worksheet formula own a typed Rust object without exposing a pointer or serialized object graph. A producer returns the object itself; a consumer accepts `Handle<T>`. xlfn owns the formula token, registry entry, and Rust value lifetime; any resource managed inside `T` remains part of `T`'s application-level contract.

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
fn dataset_evaluate(dataset: Handle<Dataset>, time: f64) -> XllResult<f64> {
    dataset.evaluate(time)
}
```

`Handle<T>` dereferences to `T` and retains an `Arc<T>` for the duration of the Rust value. A consumer therefore sees a stable object even if the producer formula replaces its object concurrently after the consumer has resolved the handle.

No `handle` argument attribute is required. Ordinary Rust trait resolution identifies `Handle<T>`.

## Re-evaluation semantics

A producer runs on every Excel evaluation. xlfn does not silently memoize the producer body.

For the same formula identity, the framework keeps the visible token and RTD topic stable, then atomically replaces the object after the refreshed observation succeeds. This yields two useful properties:

- recalculation can reload external state or rebuild the object;
- dependent cells do not receive a gratuitously different token on every recalc.

A formula identity includes the caller sheet/cell, stable UDF ID, and a canonical fingerprint of raw arguments. The fingerprint is streamed through BLAKE3 and is bounded to 16 MiB.

The caller sheet portion uses Excel's stable sheet identifier. Workbook and worksheet display names are used only to resolve that identifier and are not part of the runtime key, so renaming a sheet, renaming a workbook, or using Save As does not by itself create a new handle identity.

Changing the caller, function ID, or arguments creates a different formula identity. A producer must be deterministic enough that repeated evaluation is operationally safe; it should not create irreversible side effects merely because Excel recalculated.

## Handle alias functions

A function may return an existing `Handle<T>`:

```rust
#[excel_function(name = "CURVE.ALIAS")]
fn alias(dataset: Handle<Dataset>) -> Handle<Dataset> {
    dataset
}
```

The function republishes the same underlying `Arc<T>` under the alias formula's ownership. It does not clone the business object.

## Lifetime

The worksheet formula owns the runtime topic. The object is released when Excel removes or changes the owning formula, closes the relevant workbook dependency, terminates the RTD topic, or unloads the add-in.

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
- functions with raw reference arguments.

A producer may be `volatile`, but use volatility deliberately because it invokes construction on every recalculation cycle.

`Handle<T>` aliases also support main-thread and volatile return semantics.

## Caller restrictions

Formula ownership requires one worksheet-cell caller. Contexts without a stable single cell, such as direct VBA invocation, Function Wizard evaluation, or some multi-cell caller shapes, return a controlled error rather than creating an unowned object.

Document this behavior for users who expose handle producers in automation-heavy workbooks.

## Token security model

A token contains runtime/session identity, type identity, slot/generation data, and a keyed BLAKE3 MAC. Tokens from another process generation, tokens of the wrong type, stale slot generations, and modified tokens are rejected.

The token is a bearer capability inside the Excel process. It is not an authorization system, workbook ACL, encryption scheme, or durable serialization format. Do not parse it, persist it as an application identifier, or accept it outside the add-in's worksheet boundary.

## Object design guidance

A good handle object is:

- immutable or internally synchronized;
- cheap to share through `Arc`;
- explicit about any application-level thread affinity;
- free of workbook-owned pointers;
- bounded in memory;
- safe to drop during orderly add-in close.

