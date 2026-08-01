# Asynchronous functions

The optional `async` feature maps Rust futures to Excel's native asynchronous UDF ABI. Argument conversion occurs synchronously at the generated boundary; the future then owns its Rust inputs and completes through Excel's async-return callback.

## Enable the feature

```toml
[dependencies]
xlfn = { version = "0.1", features = ["async"] }
```

The project uses Excel 2010 or later as the operational baseline for this capability. Qualify the exact Excel versions and channels that you distribute to.

## Define an async function

```rust
#[excel_function(name = "SERVICE.FETCH")]
async fn fetch(
    #[excel_context(asynchronous)] context: AsyncContext<State>,
    symbol: String,
) -> XllResult<f64> {
    context.check_cancelled()?;
    let value = context.state().client.fetch(&key).await?;
    context.check_cancelled()?;
    Ok(value)
}
```

An async function may omit the context when it needs neither state nor cancellation:

```rust
#[excel_function(name = "TEXT.NORMALIZE")]
async fn normalize(value: String) -> String {
    value.trim().to_owned()
}
```

If a context parameter is present on an `async fn`, its role must be `asynchronous`. It is passed by value and must be the first parameter.

Async functions are registered as thread-safe by the generated boundary. They cannot accept raw Excel references or return newly constructed handle objects.

## State and converted inputs

`AsyncContext<State>` owns an `Arc<State>`. Ordinary arguments are fully converted before the future is scheduled, so `String`, `Matrix<T>`, and other owned inputs may move safely into the future. Call-scoped Excel memory never enters the executor.

The add-in controls executor size:

```rust
impl Addin for ServiceAddin {
    type State = State;
    type Error = XllError;

    fn open(_: &OpenContext) -> XllResult<State> {
        Ok(State::new())
    }

    fn async_worker_count(_: &State) -> usize {
        4
    }
}
```

The runtime clamps the count to `1..=32`. Choose it from measured workload characteristics. CPU-heavy work should usually use a dedicated bounded pool rather than occupying every async executor thread.

## Cancellation

`AsyncContext` exposes:

```rust
context.cancellation().is_cancelled();
context.cancellation_guarantee();
context.check_cancelled()?;
context.cancellation().cancelled().await;
```

Excel async calls currently receive `CancellationGuarantee::BestEffort`. The token becomes cancelled when the runtime observes the relevant Excel cancellation/lifecycle event or closes the add-in. Programmatic recalculation paths do not always produce the same calculation-event sequence, so code must not assume calculation-scoped cancellation unless the reported guarantee says so.

Cancellation is cooperative. Dropping a future or setting a token cannot forcibly interrupt:

- a blocking native call;
- synchronous filesystem or network I/O;
- a lock held by another thread;
- foreign code that does not expose cancellation.

Check the token before expensive phases and after awaited operations. Use cancellation-aware libraries where possible. Isolate truly uninterruptible work out of process when safe XLL unload is required.

The runtime linearizes cancellation against result delivery: after cancellation wins, a late completion is not delivered as a valid result to Excel.

## Do not block the async executor

A Rust `async fn` is not automatically non-blocking. This is poor:

```rust,ignore
#[excel_function(name = "DATA.FETCH")]
async fn fetch_data(
    #[excel_context(asynchronous)] context: AsyncContext<State>,
    query: String,
) -> XllResult<f64> {
    // Blocks an executor worker for the whole native call.
    context.state().native.fetch_blocking(&query)
}
```

Submit blocking or thread-affine native work to a `ThreadBoundHandle` or `ThreadBoundPoolHandle`, then await its cancellation-aware reply. See [Thread-affine native state](native-threading.md).

## Error and panic behavior

The future may return any `Result<T, E>` where `E: IntoXllError` and `T` is a valid async return type. Panics in construction, polling, conversion, or completion are contained at framework boundaries and diagnosed as internal errors.

Containment is not recovery. A panic can leave an external transaction partially complete. Keep business operations explicit and idempotent where retries are possible.

## Shutdown

On add-in close, the async manager stops accepting work, cancels tracked tasks, and waits for task guards to become idle before executor state is released. User futures and their captured values can run `Drop` during cancellation, so destructors must not block indefinitely or re-enter a resource while holding incompatible locks.

`Addin::quiesce` runs only after framework-managed async tasks have drained. Application-owned background tasks must be stopped and joined by `quiesce`; best-effort resource disposal belongs in `Addin::cleanup`.
