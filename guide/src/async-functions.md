# Asynchronous functions

Use an async UDF when one formula should receive one eventual result without
blocking Excel. Enable `async-builtin` for the built-in executor and use owned Rust inputs. For repeated updates
or formula-owned objects, see [Choosing a pattern](choosing-pattern.md).

## Enable the feature

```toml
[dependencies]
xlfn = { version = "0.2", features = ["async-builtin"] }
```

`async` provides native Excel transport, task lifecycle, cancellation, and the
executor extension API. `async-builtin` additionally provides the polling pool.
Choose only `async` when supplying an external executor.

The project uses Excel 2010 or later as the operational baseline for this capability. Qualify the exact Excel versions and channels that you distribute to.

## Define an async function

```rust
{{#include ../fixtures/async-service.md}}
#[excel_function(name = "SERVICE.FETCH")]
async fn fetch(
    context: AsyncContext<'_, ServiceAddin>,
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
{{#include ../fixtures/async-service.md}}
#[excel_function(name = "TEXT.NORMALIZE")]
async fn normalize(value: String) -> String {
    value.trim().to_owned()
}
```

If an `async fn` needs a context, use `AsyncContext<'_, A>` directly as its
first parameter, passed by value. Ordinary imports and qualified paths are
supported; type aliases, renamed imports, and custom context types are not.
Every async function must omit `thread_safe` and `macro_sheet` from
`#[excel_function]`, whether or not it has a context. Synchronous context types
cannot be used by an async function.

Async functions are registered as thread-safe by the generated boundary. They cannot accept raw Excel references or return newly constructed handle objects.

The asynchronous meaning comes from `async fn`; `AsyncContext` only adds
state and cancellation capabilities. There is no `#[excel_function(async)]`
attribute. Borrowed input
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
    context: AsyncContext<'_, ServiceAddin>,
    query: String,
) -> XllResult<f64> {
    // Blocks an executor poller for the whole external call.
    context.state().adapter.fetch_blocking(&query)
}
```

Submit blocking or thread-affine work through an application-owned bounded execution mechanism, then await an owned reply without blocking the xlfn executor. xlfn does not define that mechanism's queueing, affinity, overload, or cancellation semantics. If it is reachable from a `thread_safe` function, its concurrency contract must also satisfy the rules in [Execution modes and contexts](execution-modes.md).

## Executor capacity

Select an executor with `Addin::AsyncExecutor` and transfer it to `Opened` during
`Addin::open`. Task admission belongs to `AsyncConfig`; polling capacity belongs
to the executor:

```rust
# use xlfn::{prelude::*, AsyncConfig, AsyncPollerCount, AsyncTaskLimit,
#     BuiltinAsyncExecutor, BuiltinExecutorConfig, RuntimeConfig};
# fn example() -> XllResult<Opened<(), (), (), BuiltinAsyncExecutor>> {
# let state = ();
let runtime = RuntimeConfig::new().with_async(
    AsyncConfig::new().with_task_limit(AsyncTaskLimit::new(4096).unwrap()),
);
let executor = BuiltinAsyncExecutor::new(
    BuiltinExecutorConfig::new().with_poller_count(
        AsyncPollerCount::new(2).expect("2 is within the supported range"),
    ),
);
Ok(Opened::new(state)
    .with_runtime_config(runtime)
    .with_async_executor(executor))
# }
```

The built-in executor defaults to four pollers and accepts `1..=32`. Choose
polling capacity from measured workload characteristics. It does not configure
application connection pools, foreign runtimes, or blocking executors.

The task limit defaults to 4,096 and accepts any nonzero capacity. When that
capacity is exhausted, the framework rejects the call before converting owned
inputs; this admission error takes precedence over input errors. The early
check does not reserve capacity while custom conversion runs. After conversion,
the framework reserves executor capacity and registry admission, accounting for
concurrent submissions, cancellation, and calculation changes. Committing a
reservation transfers the opaque task to the executor without a fallible
publication step.

CPU-heavy work should usually use a separate bounded pool so it does not
occupy every async poller. See [Add-in state](lifecycle.md) for the complete
`open` pattern.

## External executors

With `async` enabled, implement `AsyncExecutor` and set
`type AsyncExecutor = YourExecutor` on the add-in. Pass its owned instance to
`Opened::with_async_executor`. An executor receives only opaque `AsyncTask<F>`
futures; Excel delivery, calculation epochs, cancellation, and handle lifetime
remain framework responsibilities.

`AsyncExecutor::reserve` may reject work before task registration. Its owned
reservation must provide everything needed by `submit`, which transfers the
task without returning an error. `start` initializes the executor during open.
`shutdown` releases its resources after all framework-owned tasks have been
destroyed. Adapters for existing runtimes can live in the application or a
separate crate; xlfn does not require Tokio.

`submit` is generic over the concrete task future:

```rust
# use xlfn::{AsyncExecutor, AsyncTask, XllResult};
# struct RuntimeHandle;
# impl RuntimeHandle {
#     fn spawn<F>(&self, task: F)
#     where
#         F: std::future::Future<Output = ()> + Send + 'static,
#     {
#         drop(task);
#     }
# }
# struct ApplicationExecutor { handle: RuntimeHandle }
# impl AsyncExecutor for ApplicationExecutor {
#     type Reservation = ();
#     fn start(&self) -> XllResult<()> { Ok(()) }
#     fn reserve(&self) -> XllResult<Self::Reservation> { Ok(()) }
fn submit<F>(&self, (): Self::Reservation, task: AsyncTask<F>)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    self.handle.spawn(task);
}
#     fn shutdown(&self) -> XllResult<()> { Ok(()) }
# }
```

The task stores its future inline so an executor can move it directly into its
own task allocation. An executor that needs a queue of different future types
can erase the type at that storage boundary with
`Pin<Box<dyn Future<Output = ()> + Send + 'static>>`. Framework task state remains
private in either case.

`NoAsyncExecutor` is available for add-ins that enable `async` for shared APIs
but do not schedule async functions. Its admission method rejects tasks.

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

Enable `handles` and either `async-builtin` or `async` with an external executor. The lease keeps the object readable across
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

During add-in shutdown, xlfn closes task admission and withdraws executor
publication, drains submission readers, closes calculation admission, and
cancels in-flight tasks. The executor stays alive to drive cancellation until
the task registry reaches zero active tasks. Only then does xlfn call
`AsyncExecutor::shutdown` and certify async quiescence. RTD and handle services
cannot be sealed or reclaimed before that certificate.

An executor that retains a task without polling or dropping it can stall close;
the framework retains the generation rather than reclaiming state still owned
by that task. Application-owned background tasks should be joined in
`Addin::quiesce`.
