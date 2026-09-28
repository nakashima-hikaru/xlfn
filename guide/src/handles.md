# Formula-owned handles

Use a handle when a worksheet formula should create a Rust object that other
formulas can reuse. Enable `handles`; add `async` only when an asynchronous
UDF consumes the object. Handle producers require a single worksheet-cell
caller and run on Excel's main thread. For alternatives, see
[Choosing a pattern](choosing-pattern.md).

## Create and use an object

Return an owned object from the producer and accept `Handle<'_, T>` in consumers:

```rust
use xlfn::{error::InputError, prelude::*};

#[derive(ExcelHandleObject)]
pub struct Dataset {
    values: Vec<f64>,
}

impl Dataset {
    fn total(&self) -> f64 {
        self.values.iter().sum()
    }
}

#[excel_function(name = "DATASET.CREATE")]
fn create_dataset(values: Row<f64>) -> XllResult<Dataset> {
    let values = values.into_vec();
    if values.is_empty() {
        return Err(XllError::input(
            "values",
            InputError::Malformed("at least one value is required"),
        ));
    }
    Ok(Dataset { values })
}

#[excel_function(name = "DATASET.TOTAL", thread_safe)]
fn dataset_total(dataset: Handle<'_, Dataset>) -> f64 {
    dataset.total()
}
```

In Excel, put `=DATASET.CREATE(A1:C1)` in a cell, then pass that cell to
`=DATASET.TOTAL(D1)` if the producer is in `D1`. The producer cell displays an
opaque token. Pass the token through formulas; do not interpret its text.

## Define a handle object

`ExcelHandleObject` requires `Send + Sync + 'static`. The object may contain
immutable data, synchronized application clients, typed resource identifiers,
or other owned Rust values. It must not contain call-scoped Excel references.
xlfn manages the Rust object's lifetime; resources inside it remain subject
to the application's own concurrency and shutdown rules.

## Borrow an object in a consumer

`Handle<'call, T>` dereferences to `T` and stays valid for the active Excel
call. It is not `Clone` and cannot be stored in add-in state, another handle
object, or an async task. No `handle` argument attribute is required: the
parameter type selects handle input conversion.

## Re-evaluation semantics

The producing cell keeps an object for its current formula and converted
inputs. Recalculating that same formula reuses the object without calling the
producer again. Changing an input creates a new object and token. Renaming a
sheet or workbook alone does not recreate the object.

When the object depends on external data, add a version or snapshot ID as a
formula argument. For example, a `DATASET.LOAD` function could accept a
snapshot ID from `A1`; updating `A1` then creates a new object.

If a custom argument type is used by a handle producer, implement
`ExcelInputIdentity` and `PrepareExcel` alongside `FromExcel`; see [Custom conversions](https://github.com/nakashima-hikaru/xlfn/blob/main/docs/reference/custom-conversions.md).

## Handle alias functions

A function may explicitly republish an existing handle through `HandleAlias`:

```rust
#[excel_function(name = "DATASET.ALIAS")]
fn alias(dataset: Handle<'_, Dataset>) -> HandleAlias<'_, Dataset> {
    dataset.alias()
}
```

The new formula refers to the same Rust object. `HandleAlias<'_, T>` is a
call-scoped return value; a plain `Handle<'_, T>` is an input and cannot be
returned or stored after the call.

## Async-scoped handle leases

An async UDF that needs an existing object accepts a generation-scoped lease:

```rust
#[excel_function(name = "DATASET.ASYNC.EVALUATE")]
async fn async_evaluate(
    dataset: HandleLease<'_, Dataset>,
) -> XllResult<f64> {
    std::future::ready(()).await;
    Ok(dataset.total())
}
```

`HandleLease<'generation, T>` is supplied by the generated async boundary.
It keeps the object readable before and after `.await` until that task ends.
The lease cannot be returned, stored in `'static` state, or moved into an
independently spawned thread. See [Async handle inputs](async-functions.md#async-handle-inputs).

`HandleLease` is an async input, not an Excel return value. A saved token can
look up an object only while its producing formula remains active.

## Lifetime

After the last formula using an object is removed, it remains alive until
active calls and async tasks finish using it. Removal can therefore happen
before the object's `Drop` runs. Destruction may occur on several threads;
do not rely on `Drop` for thread-affine application cleanup.

Destructors must obey the same shutdown rules as any in-process code:

- do not call Excel;
- do not block indefinitely;
- do not panic;
- do not directly destroy a thread-affine application resource from an arbitrary handle destructor.

The default limit is 16,384 live formula bindings per open generation.
Configure it with `RuntimeConfig::with_handles` and
`HandleConfig::with_binding_limit`; `HandleBindingLimit` accepts `1..=1_048_576`.
A new publication may also return `Overloaded` while earlier calls are still
using removed objects. Let those calls finish before retrying.

Internally, binding slots and their free list use `u32` identities. Each object
also has separate `u32` binding and async-pin counts: their portable ceiling is
`u32::MAX` on both 32-bit and 64-bit hosts. An admission that would exceed either
count returns an overflow error without creating a capability or changing the
other count. Aggregate arena bookkeeping remains native-sized.

## Shutdown interaction

The close order relevant to handle objects is:

1. xlfn stops and drains framework-managed work;
2. `Addin::quiesce` establishes application-level quiescence;
3. xlfn closes the formula-handle registry and drops remaining Rust handle objects;
4. `Addin::cleanup` performs bounded best-effort disposal.

A handle object's `Drop` therefore must remain safe after `quiesce` has stopped application workers or owner threads. If resource destruction requires such an owner, release or invalidate the resource during `quiesce` while the owner is still available, and make the later Rust wrapper drop a local or idempotent operation. Do not defer the only copy of an application shutdown protocol to handle `Drop`. See [Shutdown and unload](https://github.com/nakashima-hikaru/xlfn/blob/main/docs/reference/shutdown.md).

## Valid producer contexts

A newly constructed handle object uses main-thread return semantics. Producers cannot be:

- thread-safe UDFs;
- macro-sheet UDFs;
- asynchronous UDFs;
- functions with raw reference arguments;
- volatile UDFs.

`HandleAlias<'_, T>` uses main-thread return semantics. A borrowed
`Handle<'_, T>` is an input capability only and is not a valid return type.

`HandleLease<'_, T>` is limited to generated async UDF inputs. It cannot be
returned to Excel or used to keep an object alive independently of its task.

## Caller restrictions

Formula ownership requires one worksheet-cell caller. Contexts without a stable single cell, such as direct VBA invocation, Function Wizard evaluation, or some multi-cell caller shapes, return a controlled error rather than creating an unowned object.

Document this behavior for users who expose handle producers in automation-heavy workbooks.

## Handle token lifetime

Tokens are valid only while the producing formula owns its object in the
current Excel session. Expired, modified, or wrong-type tokens are rejected.

Use tokens only as worksheet handle inputs. Do not parse them, store them as
persistent application IDs, or send them to an external service.
