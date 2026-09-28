# Streaming RTD

Use Real-Time Data (RTD) when a formula should receive repeated updates from a
push source. Enable `rtd`; enabling `handles` alone does not expose this API.
Subscribe from a main-thread UDF and provide scalar values. See
[Choosing a pattern](choosing-pattern.md) if the formula needs only one future
result or a reusable Rust object.

## Create a producer

Use `RtdChannelSource` for ordinary producers. The source owns a factory that
runs synchronously during subscription setup and returns an owned producer
job for the topic. Each job runs once on a framework-owned thread and receives
only a typed, bounded `RtdSender`. Its captures may include resources that are
neither `Clone` nor `Sync`. A generation-owned shared publisher forwards accepted values to Excel.
The capacity passed to `RtdChannelSource::new` limits queued values per subscription.

```rust
{{#include ../../examples/rtd-source/src/metric_source.rs}}
```

In this illustrative loop, `try_next_metric` is a non-blocking poll and
`wait_closed` makes the polling delay interruptible. Use bounded or
cancellation-aware I/O so the callback returns promptly after admission
closes. The factory validates the topic before any worker starts. Producer
errors and panics close the channel and are reported during disconnect.

Each active subscription uses one producer thread. All channel subscriptions in
a runtime generation share one publisher, for N+1 threads in total. The publisher
processes at most 32 values per topic turn before yielding to another ready
topic. A stalled publication can delay other topics on the shared worker.

## Register and subscribe

Keep the source handle in add-in state and subscribe from a main-thread function:

```rust
{{#include ../../examples/rtd-source/src/lib.rs}}
```

The source handle is the opaque RTD identity. Clone a handle when multiple
functions should refer to the same source; registering a new source creates a
distinct identity even when its value is equivalent. The runtime owns the
source and subscription identity; handles do not extend its lifetime.
Formulas using the same source handle and topic share an active subscription.
A failed new subscription attempt does not cancel an established subscriber. The complete compile-tested fixture,
including the add-in state and `Client` placeholder, lives under
`examples/rtd-source`.

The first call returns the current value, which can be empty while the
producer starts. Later publications update the topic and notify Excel.

## RTD value types

`RtdValue` supports scalar transport:

- finite number;
- Boolean;
- integer;
- string;
- Excel error;
- empty.

`IntoRtdValue` is implemented for common scalar types, including `f64`, `bool`, `i32`, exactly representable `i64`, `ExcelSerialDate`, strings, `ExcelErrorValue`, and `()`.

A custom converter returns an owned `RtdValue`. The sender checks finite
numbers and Excel's string-length limit before queueing it.

RTD does not transport arrays. Publish a handle or another scalar identity and expose a separate function when a stream logically updates a complex object.

## Define a topic

```rust
let parts = [
    "events",
    topic.as_str(),
    metric.as_str(),
];
context.rtd().subscribe(&source_handle, &parts)?;
```

For one part:

```rust
context.rtd().subscribe(&source_handle, &["service-health"])?;
```

Topic parts identify the subscription. Use stable, canonical values rather than
display labels. The call borrows its parts; source callbacks receive an owned
`RtdTopic`.

Use `topic.len()` to inspect the part count, `topic.part(index)` for one part,
and `topic.parts()` to iterate over all parts.

## Topic and capacity limits

A topic must contain at least one non-empty part. Each part must fit Excel's 32,767 UTF-16-unit counted-string representation.

The runtime also applies bounded admission limits. The standard limits are 253 topic parts, 1 MiB of UTF-8 text per topic, 64 MiB of pending-topic text in aggregate, 4,096 pending preparations, 4,096 active streams, 4,096 queued updates, and 4,096 distinct live source identities. Configure limits during `Addin::open` with `RuntimeConfig::new().with_rtd(RtdConfig::new().with_limits(limits))`; use `RtdCapacity::bounded` or `RtdCapacity::disabled` for each resource class so a disabled limit is explicit rather than an untyped zero. Exceeding a limit returns `XllError::Overloaded` (or a topic input error for an invalid topic).

## Backpressure and errors

`RtdSender::try_send` validates values before enqueueing and never waits for
queue capacity. `RtdSendError::Full` and `RtdSendError::Closed` retain the
validated update as `RtdPendingValue<T>`. Recover it with `into_pending()` and retry
on any `RtdSender<T>` of the same input type with `try_send_pending()`, without
cloning the original input or re-running its
converter. Use a bounded retry policy and stop when the channel closes.
`RtdSendError::Invalid` contains a conversion/validation error and no payload.
Conversion happens before admission, including when the channel is closed.
Calling `.map_err(RtdSendError::into_error)?` deliberately discards its retained
payload and maps Full/Closed to Overloaded/Closing. There is no implicit lossy
`From` conversion. Enqueue success is not a
delivery acknowledgement: disconnect can discard pending values. If the
publisher encounters a runtime error, it closes admission, wakes the producer,
and reports that error during disconnect. `Closing` is treated as normal
shutdown.

Publishing validates and queues a value; xlfn notifies Excel and handles
`RefreshData`.

## Stop a producer

During disconnect, the channel stops accepting values, discards queued
updates, waits for in-flight publication, revokes its sink, and joins its
producer. The generation joins the shared publisher after all subscriptions
have disconnected. Sender clones may outlive the
subscription, but can only return `XllError::Closing`; they retain no live RTD
capability or queued payloads. Producer captures belong to that job and are
dropped before its worker is joined. Dropping the source configuration does
not stop or destroy a running job.

A producer that finishes successfully closes sender admission and drains
accepted values. The shared publisher remains available to other topics. The producer must stop any additional threads or callbacks
before returning. Use bounded, cancellation-aware I/O: a producer that never
returns delays disconnect and unload. Never abandon an in-process callback
on timeout and then permit unload; isolate uninterruptible work in another
process. See [Shutdown and unload](https://github.com/nakashima-hikaru/xlfn/blob/main/docs/reference/shutdown.md).

## Custom sources

If `RtdChannelSource` cannot represent your producer, the unsafe `RtdSource`
and `RtdSubscription` traits offer a custom integration path. Read their
complete safety contracts in the API documentation before implementing them.

## Temporary COM registration

The XLL registers its RTD COM server temporarily. On a later start after an
abnormal Excel exit, xlfn cleans up stale registrations belonging to the same
XLL.

Installation still needs appropriate user-profile registry access. Test start, normal close, forced Excel termination, and restart in the deployment environment.

See [Deployment](deployment.md) for installation. Authenticate external feeds
in the application that supplies RTD data.
