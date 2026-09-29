# Asynchronous functions

Use an async UDF when one formula should receive one eventual result without
blocking Excel. Enable `async` and use owned Rust inputs. For repeated updates
or formula-owned objects, see [Choosing a pattern](choosing-pattern.md).

## Enable the feature

```toml
[dependencies]
xlfn = { version = "0.2", features = ["async"] }
```

The project uses Excel 2010 or later as the operational baseline for this capability. Qualify the exact Excel versions and channels that you distribute to.

## Define an async function

```rust
{{#include ../fixtures/async-service.md}}
#[excel_function(name = "SERVICE.FETCH")]
async fn fetch(
    #[excel_context(asynchronous)] context: AsyncContext<'_, ServiceAddin>,
    key: String,
) -> XllResult<f64> {
    context.check_cancelled()?;
    let value = context.state().client.fetch(&key).await?;
    context.check_cancelled()?;
    Ok(value)
}
```

An async function may omit the context when it needs neither state nor cancellation:

```rust
{{#include ../fixtures/addin.md}}
# use xlfn::prelude::*;
#[excel_function(name = "TEXT.NORMALIZE")]
async fn normalize(value: String) -> String {
    value.trim().to_owned()
}
```

If a context parameter is present on an `async fn`, its role must be `asynchronous`. It is passed by value and must be the first parameter.

Async functions are registered as thread-safe by the generated boundary. They cannot accept raw Excel references or return newly constructed handle objects.

The asynchronous meaning comes from the Rust function being written as
`async fn`; there is no `#[excel_function(async)]` attribute. Borrowed input
types such as `&str`, `MatrixRef<'_, T>`, `ExcelCellRef<'_>`, `XlStrRef<'_>`,
and `XlArrayRef<'_>` are rejected at compile time because their call scope
ends before the future may run. Use owned `String`, `Matrix<T>`, or another
`Send + 'static` representation instead.

## State and converted inputs

Arguments are converted while Excel is making the call. The scheduled future
owns ordinary inputs such as `String` and `Matrix<T>`; borrowed Excel memory
cannot outlive that call and is rejected in async signatures.

`AsyncContext<'_, A>` lets the invocation borrow shared state and observe
cancellation. The framework keeps that state alive until the task ends. Do not
move the context into a detached task that could outlive the invocation.

## Cancellation

`AsyncContext` exposes:

```rust,no_run
{{#include ../fixtures/async-service.md}}
# async fn example(context: AsyncContext<'_, ServiceAddin>) -> XllResult<()> {
context.cancellation().is_cancelled();
context.cancellation_guarantee();
context.check_cancelled()?;
context.cancellation().cancelled().await;
# Ok(())
# }
```

Excel async calls currently receive `CancellationGuarantee::BestEffort`. The token becomes cancelled when the runtime observes the relevant Excel cancellation/lifecycle event or closes the add-in. Programmatic recalculation paths do not always produce the same calculation-event sequence, so code must not assume calculation-scoped cancellation unless the reported guarantee says so.

Cancellation is cooperative. Dropping a future or setting a token cannot forcibly interrupt:

- a blocking or foreign call;
- synchronous filesystem or network I/O;
- a lock held by another thread;
- foreign code that does not expose cancellation.

Check the token before expensive phases and after awaited operations. Use cancellation-aware libraries where possible. Isolate truly uninterruptible work out of process when safe XLL unload is required.

The runtime linearizes cancellation against result delivery: after cancellation wins, a late completion is not delivered as a valid result to Excel.

## Do not block the async executor

A Rust `async fn` is not automatically non-blocking. This is poor:

```rust,no_run
{{#include ../fixtures/async-service.md}}
#[excel_function(name = "DATA.FETCH")]
async fn fetch_data(
    #[excel_context(asynchronous)] context: AsyncContext<'_, ServiceAddin>,
    query: String,
) -> XllResult<f64> {
    // Blocks an executor worker for the whole external call.
    context.state().adapter.fetch_blocking(&query)
}
```

Submit blocking or thread-affine work through an application-owned bounded execution mechanism, then await an owned reply without blocking the xlfn executor. xlfn does not define that mechanism's queueing, affinity, overload, or cancellation semantics. If it is reachable from a `thread_safe` function, its concurrency contract must also satisfy the rules in [Execution modes and contexts](execution-modes.md).

## Executor capacity

Configure the pool in `Addin::open` with a runtime policy:

```rust
# use xlfn::{prelude::*, AsyncConfig, AsyncWorkerCount, RuntimeConfig};
# fn example() -> XllResult<Opened<()>> {
# let state = ();
let runtime = RuntimeConfig::new().with_async(
    AsyncConfig::new().with_worker_count(
        AsyncWorkerCount::new(4).expect("4 is within the supported range"),
    ),
);
Ok(Opened::new(state).with_runtime_config(runtime))
# }
```

Import `AsyncConfig`, `AsyncWorkerCount`, and `RuntimeConfig` from `xlfn`.
The worker count defaults to four and accepts `1..=32`. Choose it from
measured workload characteristics. This setting does not configure any
application-owned connection pool, foreign runtime, or blocking executor.
CPU-heavy work should usually use a separate bounded pool so it does not
occupy every async executor worker. See [Add-in state](lifecycle.md) for the
complete `open` pattern.

## Async handle inputs

An async UDF that needs a formula-owned object must accept `HandleLease<'_, T>`, not
`Handle<'_, T>`:

```rust
{{#include ../fixtures/dataset.md}}
#[excel_function(name = "DATASET.ASYNC_EVALUATE")]
async fn async_evaluate(dataset: HandleLease<'_, Dataset>, time: f64) -> XllResult<f64> {
    std::future::ready(()).await;
    dataset.evaluate(time)
}
```

Enable both `handles` and `async`. The lease keeps the object readable across
`.await` for this framework-managed task. It is released when the task
completes, is cancelled, panics, or is dropped during shutdown. It cannot be
returned, stored in `'static` state, or moved into an independently spawned
thread. `Handle<'_, T>` remains synchronous and call-scoped.

There is no `Handle::pin()` escape from that scope. For synchronous object
sharing, use another formula binding through `HandleAlias`; see
[Formula-owned handles](handles.md).

## Error and panic behavior

The future may return any `Result<T, E>` where `E: IntoXllError` and `T` is a
valid async return type. Panics are contained at framework boundaries and reported
as internal diagnostic errors.

## Shutdown

During add-in shutdown, xlfn stops accepting new async tasks, cancels in-flight
work, and drains active tasks before `Addin::quiesce` runs. Application-owned
background tasks should be joined in `Addin::quiesce`.
