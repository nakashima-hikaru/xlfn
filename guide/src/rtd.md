# Streaming RTD

Real-Time Data (RTD) is the appropriate model for a formula that should update repeatedly from a push source. xlfn hides the COM server transport and exposes typed sources, topics, sinks, and subscriptions.

## Data flow

```text
worksheet formula
    -> MainThreadContext::rtd().subscribe(&source_handle, topic)
    -> RtdChannelSource starts a producer and publisher
    -> producer calls sender.try_send(value)
    -> framework publisher forwards queued values through RtdSink
    -> framework batches RefreshData
    -> Excel recalculates the dependent formula
```

The initial subscription returns the current value, which can be empty while
the asynchronous producer starts. Later publications update the RTD topic and
notify Excel.

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

The runtime also applies bounded admission limits. The standard limits are 253 topic parts, 1 MiB of UTF-8 text per topic, 64 MiB of pending-topic text in aggregate, 4,096 pending preparations, 4,096 active streams, 4,096 queued updates, and 4,096 distinct live source identities. A custom `RuntimeConfig::with_rtd_limits` can choose lower limits during `Addin::open`; use `RtdCapacity::bounded` or `RtdCapacity::disabled` for each resource class so a disabled limit is explicit rather than an untyped zero. Exceeding a limit returns `XllError::Overloaded` (or a topic input error for an invalid topic).

## Implement a source

Use `RtdChannelSource` for ordinary producers. Its callback runs on a
framework-owned thread and receives only a typed, bounded `RtdSender`. A
separate publisher thread owns the internal sink. The capacity passed to
`RtdChannelSource::new` limits queued values per subscription.

```rust
{{#include ../../examples/rtd-source/src/metric_source.rs}}
```

In this illustrative loop, `try_next_metric` is a non-blocking poll and
`wait_closed` makes the polling delay interruptible. Use bounded or
cancellation-aware I/O so the callback returns promptly after admission
closes. Callback errors and panics close the channel and are reported during
disconnect; topic validation inside the callback also runs asynchronously.

## Shutdown and advanced sources

The channel adapter closes admission, discards pending values, and joins both
workers during disconnect. Sender clones can remain alive afterward: their
`try_send` calls return `XllError::Closing`, even after the RTD runtime has
been reclaimed. They own only channel state and never receive a raw sink.
Successful producer completion drains accepted values before the publisher
stops. The producer must stop any additional threads or callbacks before it
returns. A producer that ignores cancellation delays disconnect and unload.

Each active channel subscription uses two threads. Advanced integrations that
share an event loop can implement the existing unsafe `RtdSource` and
`RtdSubscription` traits. In that path, a sink must not escape an `Err` or
panic from `subscribe`; the returned subscription must stop every sink user
before `disconnect_and_wait` returns. Cancellation must be bounded,
idempotent, panic-free, and must not call Excel or re-enter framework
subscription APIs. Sinks are non-owning capabilities, so this shutdown
contract is a memory-safety requirement for the unsafe extension point.

Do not implement a timeout that abandons an in-process callback and then permits the XLL to unload. Put uninterruptible producers in another process.

## Use the source from a function

Keep the source in add-in state and subscribe from a main-thread function:

```rust
{{#include ../../examples/rtd-source/src/lib.rs}}
```

The source handle is the opaque RTD identity. Clone a handle when multiple
functions should refer to the same source; registering a new source creates a
distinct identity even when its value is equivalent. The runtime owns the
source and subscription identity; handles do not extend its lifetime.
Multiple formulas that observe the same active subscription share it; a failed
new observation rolls back only the reservation created by that attempt, not
an unrelated established subscriber. The complete compile-tested fixture,
including the add-in state and `Client` placeholder, lives under
`examples/rtd-source`.

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

`RtdSender::try_send` validates values before enqueueing and never waits for
queue capacity. It returns `XllError::Overloaded` when the per-subscription
queue is full and `XllError::Closing` after admission closes. Handle an error
by stopping or retrying with a bounded policy. Enqueue success is not a
delivery acknowledgement: disconnect can discard pending values. If the
publisher encounters a runtime error, it closes admission, wakes the producer,
and reports that error during disconnect. `Closing` is treated as normal
shutdown.

For an unsafe custom source, `RtdSink::publish` directly reports a closing
runtime, an inactive subscription, invalid values, or exhaustion of the
runtime's queued-update limit. Do not loop tightly on a permanent error.

Publishing validates and queues a value; notification and `RefreshData` happen through the framework. User code must never call the COM update event directly.

## Temporary COM registration

The XLL registers its RTD COM server temporarily. Ownership markers include the add-in identity, schema, module path, and CLSID. On a later start after an abnormal Excel exit, xlfn removes only stale registrations whose full marker set belongs to the same XLL. It does not broadly delete similarly named registry keys.

Installation still needs appropriate user-profile registry access. Test start, normal close, forced Excel termination, and restart in the deployment environment.

## Choosing RTD versus async

Use async for one eventual result. Use RTD for a value that may change repeatedly while a formula remains subscribed. Do not simulate streaming by starting an infinite async function; it prevents normal completion and complicates cancellation and unload.
