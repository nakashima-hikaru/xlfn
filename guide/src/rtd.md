# Streaming RTD

Real-Time Data (RTD) is the appropriate model for a formula that should update repeatedly from a push source. xlfn hides the COM server transport and exposes typed sources, topics, sinks, and subscriptions.

## Data flow

```text
worksheet formula
    -> MainThreadContext::subscribe(source, topic)
    -> RtdSource::subscribe(topic, sink)
    -> background producer calls sink.publish(value)
    -> framework batches RefreshData
    -> Excel recalculates the dependent formula
```

The initial subscription returns a current value. Later publications update the RTD topic and notify Excel.

## Define a topic

```rust
let topic = RtdTopic::new([
    "events",
    symbol.as_str(),
    field.as_str(),
])?;
```

For one part:

```rust
let topic = RtdTopic::single("service-health")?;
```

A topic must contain at least one non-empty part. Each part must fit Excel's 32,767 UTF-16-unit counted-string representation. Topic parts are identity, not display labels; use stable, canonical values.

The runtime also applies bounded admission limits. The standard limits are 253 topic parts, 1 MiB of UTF-8 text per topic, 64 MiB of pending-topic text in aggregate, 4,096 pending preparations, 4,096 active streams, 4,096 queued updates, and 4,096 distinct live source identities. A custom `Runtime::new_with_rtd_limits` can choose lower limits for a deployment; exceeding a limit returns `XllError::Overloaded` (or a topic input error for an invalid topic).

## Implement a source

```rust
{{#include ../../examples/rtd-source/src/metric_source.rs}}
```

The source's subscription path must be bounded. It may create a worker or register with an existing event loop, but it should not wait indefinitely for the first external message. In this illustrative loop, `try_next_metric` is a non-blocking poll; in production, prefer a cancellation-aware channel or event loop over periodic polling.

## Implement shutdown correctly

```rust
{{#include ../../examples/rtd-source/src/metric_subscription.rs}}
```

`RtdSubscription` is an unsafe trait because this implementation controls
whether the XLL can be unloaded. The `unsafe impl` is justified only by the
two-part quiescence guarantee shown above: cancellation stops the sole
producer, and `disconnect_and_wait` joins it before returning.

`request_cancel` must be non-blocking, idempotent, panic-free, and must not call Excel or re-enter framework subscription APIs. `disconnect_and_wait` performs the quiescence barrier: when it returns, the source must no longer execute XLL code or publish through the sink.

Do not implement a timeout that abandons an in-process callback and then permits the XLL to unload. Put uninterruptible producers in another process.

## Use the source from a function

Keep the source in add-in state and subscribe from a main-thread function:

```rust
{{#include ../../examples/rtd-source/src/lib.rs:42:48}}
```

The source is typically an `Arc<S>`. Source sharing uses `Arc` allocation identity, equivalent to `Arc::ptr_eq`, combined with the logical RTD topic. Cloning the same `Arc` shares the subscription; constructing a new `Arc`, even around an equivalent value, creates a distinct source identity. Multiple formulas that observe the same active subscription share it; a failed new observation rolls back only the reservation created by that attempt, not an unrelated established subscriber. The complete compile-tested fixture, including the add-in state and `Client` placeholder, lives under `examples/rtd-source`.

## RTD value types

`RtdValue` supports scalar transport:

- finite number;
- Boolean;
- integer;
- string;
- Excel error;
- empty.

`IntoRtdValue` is implemented for common scalar types, including `f64`, `bool`, `i32`, exactly representable `i64`, `ExcelSerialDate`, strings, `ExcelErrorValue`, and `()`.

RTD does not transport arrays. Publish a handle or another scalar identity and expose a separate function when a stream logically updates a complex object.

## Backpressure and errors

`RtdSink::publish` can fail when the runtime is closing, the subscription is no longer active, the value is invalid, or the queued-update limit is exhausted. A producer must handle the error and stop or retry with a bounded policy. Do not loop tightly on a permanent error.

Publishing validates and queues a value; notification and `RefreshData` happen through the framework. User code must never call the COM update event directly.

## Temporary COM registration

The XLL registers its RTD COM server temporarily. Ownership markers include the add-in identity, schema, module path, and CLSID. On a later start after an abnormal Excel exit, xlfn removes only stale registrations whose full marker set belongs to the same XLL. It does not broadly delete similarly named registry keys.

Installation still needs appropriate user-profile registry access. Test start, normal close, forced Excel termination, and restart in the deployment environment.

## Choosing RTD versus async

Use async for one eventual result. Use RTD for a value that may change repeatedly while a formula remains subscribed. Do not simulate streaming by starting an infinite async function; it prevents normal completion and complicates cancellation and unload.
