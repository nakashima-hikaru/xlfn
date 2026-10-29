# Streaming RTD

Use Real-Time Data (RTD) when a formula should receive repeated updates from a
push source. Enable `rtd`; enabling `handles` alone does not expose this API.
Subscribe from a main-thread UDF and provide scalar values. See
[Choosing a pattern](choosing-pattern.md) if the formula needs only one future
result or a reusable Rust object.

## Share a connection across topics

Use `RtdMultiplexedSource` when one connection or event loop serves many
topics. Its owned job starts on one framework-owned worker when the first
subscription connects. Further subscriptions receive their own typed,
bounded `RtdSender` without starting more producer threads. The job can own
resources that are neither `Clone` nor `Sync`.

The context receives `Subscribe { id, topic, sender }` and `Unsubscribe { id }`
events. Track subscriptions by their opaque `RtdSubscriptionId`: reconnecting
the same topic creates a new ID, so an old removal cannot remove a new
subscription. Multiple functions can share the same registered source handle.

```rust
{{#include ../../examples/rtd-source/src/multiplexed_metric_source.rs}}
```

`Client` is a placeholder for a shared connection. Its operations must be
bounded or cancellation-aware; `try_next_update` is non-blocking. The loop
bounds both control-event and data processing per turn. Updates are routed
by symbol without scanning idle topics. Its explicit overload policy drops
a sampled update when the shared buffering budget is exhausted.

The optional topic validator runs synchronously during subscription setup.
Use it for inexpensive validation, with network initialization in the worker.
A validation failure rejects that subscription without stopping the source.

The source worker stays available when its last topic disconnects and is
joined at runtime shutdown. A completed source is terminal: a successful
return closes sender admission and drains accepted values; it does not
restart on a later subscription. Errors and panics apply the configured
producer error policy to connected topics. `RetainLastValue` is the default;
`PublishError` drains accepted data, then publishes an Excel error using one
reserved terminal slot per subscription. A successful return adds no error.
A completed source retains its runtime worker reservation until shutdown
joins the exited worker.

## Independent blocking producers

Use `RtdChannelSource` when each topic needs its own blocking job. Its factory
runs synchronously during subscription setup and returns an owned job that
runs once on its own worker. The factory should validate and prepare captures;
perform bounded or cancellation-aware I/O in the job.

```rust
{{#include ../../examples/rtd-source/src/metric_source.rs}}
```

This adapter creates one producer thread per active subscription. It admits
at most 64 retained producer workers per source by default; set a different
nonzero bound with `.with_max_producers(limit)`. A reservation lasts until
disconnect joins the worker, even if the job has already exited. Lowering the
bound preserves existing reservations and rejects new subscriptions until
enough workers are joined. The runtime's worker limit additionally applies
across all blocking and multiplexed sources.

Both adapters share one publisher and one cleanup worker per runtime
generation, plus one notification worker per started Excel RTD server on
Windows. The publisher processes at most 32 values per topic turn before
yielding to another ready topic. A stalled publication can delay other topics
on that worker.

## Register and subscribe

Keep the source handle in add-in state and subscribe from a main-thread function:

```rust
{{#include ../../examples/rtd-source/src/lib.rs}}
```

Place `#[excel_addin]` on the `Addin` implementation. The example selects
`type RtdSubscription = BoxedRtdSubscription;` and calls
`context.rtd().register(source)`. The returned `RtdSourceHandle` hides the
producer, closure, value and subscription implementation types from state.
The macro supplies the dynamic catalog when `RtdSources` is omitted.

Each registered source has one boxed adapter; each boxed subscription has
one allocation and delegates cancellation and its consuming shutdown barrier
to the original subscription. Sources keep their existing bounded queues,
worker limits and backpressure policies. This type erasure changes dispatch,
not lifecycle ownership.

The macro
checks the feature enabled in `xlfn` itself; do not put a consumer-side
`#[cfg(feature = "rtd")]` on these associated types. Explicit RTD configuration
requires enabling the dependency's `rtd` feature.

Add-ins with no RTD sources omit both declarations. This remains true when
`rtd` is enabled for the framework, including through Cargo feature unification.
The macro supplies empty types as needed. With `rtd` disabled, the RTD
associated types and public RTD API are absent. `handles` alone uses an internal
RTD transport and requires no application RTD configuration.

The source handle is the opaque RTD identity. Clone a handle when multiple
functions should refer to the same source; registering a new source creates a
distinct identity even when its value is equivalent. The runtime owns the
source and subscription identity; handles do not extend its lifetime.
The runtime checks both the registration identity and lifecycle generation,
so a handle from another add-in cannot resolve a coincidentally numbered source.

Declare one to sixteen known source types with
`Addin::RtdSources = (SourceA, SourceB)` and
register the complete tuple once with
`context.rtd().register_sources((source_a, source_b))?`. The returned handles
have the same tuple order. The runtime stores the concrete sources together
and dispatches through their types, without a separate boxed adapter for each
source. A one-source catalog uses `(Source,)`, including the trailing comma.

For a source set assembled incrementally, or source types containing captured
closures that cannot be named in an associated type, select
`type RtdSources = xlfn::DynamicRtdSources<Self::RtdSubscription>;`.
This mode also retains `register_source(source)` for typed handles and common
subscription owners. It owns a boxed adapter per source. A typed handle can
be stored without its concrete source type by calling `.erase()`; the erased
handle keeps the exact source identity and generation. Both forms can be
passed to `subscribe`. Erasure cannot issue a source identity or transfer a
handle to another generation.

`context.rtd().subscribe(&source, &topic)` accepts an existing `RtdTopic`
directly. Arrays, slices, and vectors of `String` or string references are
also accepted, so `Vec<String>` does not need a temporary `Vec<&str>`. These
inputs borrow stable storage; an existing subscription reuses its canonical
topic without allocating a new copy. Convert arbitrary iterators or custom
string representations with `RtdTopic::new` before subscribing.

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

`IntoRtdValue` is implemented for common scalar types, including `f64`, `bool`, `i32`, exactly representable `i64`, `ExcelSerialDate`, strings, `ExcelError`, and `()`.

A custom converter returns an owned `RtdValue`. The sender checks finite
numbers and Excel's string-length limit before queueing it.

RTD does not transport arrays. Publish a handle or another scalar identity and expose a separate function when a stream logically updates a complex object.

## Define a topic

```rust
{{#include ../fixtures/app.md}}
# use xlfn::rtd::{RtdSource, RtdSourceHandle};
# fn example<S: RtdSource>(context: MainThreadContext<'_, AppTools>, source_handle: RtdSourceHandle<S>, topic: String, metric: String) -> XllResult<()> {
let parts = [
    "events",
    topic.as_str(),
    metric.as_str(),
];
context.rtd().subscribe(&source_handle, &parts)?;
# Ok(())
# }
```

For one part:

```rust
{{#include ../fixtures/app.md}}
# use xlfn::rtd::{RtdSource, RtdSourceHandle};
# fn example<S: RtdSource>(context: MainThreadContext<'_, AppTools>, source_handle: RtdSourceHandle<S>) -> XllResult<()> {
context.rtd().subscribe(&source_handle, &["service-health"])?;
# Ok(())
# }
```

Topic parts identify the subscription. Use stable, canonical values rather than
display labels. The call borrows its parts; source callbacks receive an owned
`RtdTopic`.

Use `topic.len()` to inspect the part count, `topic.part(index)` for one part,
and `topic.parts()` to iterate over all parts.

## Topic and capacity limits

A topic must contain at least one non-empty part. Each part must fit Excel's 32,767 UTF-16-unit counted-string representation.

The runtime also applies bounded admission limits. The standard limits are
253 topic parts, 1 MiB of UTF-8 text per topic, 64 MiB of pending-topic text
in aggregate, 4,096 pending preparations, 4,096 active streams, 4,096 dirty
topic updates awaiting refresh, and 4,096 distinct live source identities.
Configure limits during `Addin::open` with
`RuntimeConfig::new().with_rtd(RtdConfig::new().with_limits(limits))`.
Use `RtdCapacity::bounded` or `RtdCapacity::disabled` for each resource class;
zero never means unlimited. Exceeding a limit returns `XllError::Overloaded`
(or a topic input error for an invalid topic).

Adapter buffering and worker limits are separate from dirty refresh topics:

| Resource | Default | Configuration |
| --- | --- | --- |
| Subscriptions and retained lifecycle reservations per source | 4,096 | `RtdSourceLimits::with_max_subscriptions` |
| Buffered data values per source | 4,096 | `RtdSourceLimits::with_max_buffered_values` |
| Buffered data bytes per source | 64 MiB | `RtdSourceLimits::with_max_buffered_bytes` |
| Buffered data values across the runtime | 4,096 | `RtdLimits::with_max_buffered_values` |
| Buffered data bytes across the runtime | 64 MiB | `RtdLimits::with_max_buffered_bytes` |
| Retained producer workers across the runtime | 64 | `RtdLimits::with_max_workers` |

Set source limits with `.with_limits(RtdSourceLimits::standard()...)`.
Buffered values remain charged while a publication batch is in flight. Byte
accounting covers each stored value representation and its owned UTF-8 string
payload; it excludes spare queue capacity, allocator overhead, conversion
temporaries, upstream buffers, and Excel-side storage. Each subscription has
one additional fixed terminal-error slot. Worker admission counts a shared
source once and a blocking source once per producer, including exited workers
not yet joined. Publisher, cleanup, and COM notification workers are separate
fixed infrastructure.

A multiplexed subscription retains its source reservation through channel
revocation and then until the worker dequeues its `Unsubscribe` event. When
the source stops, queued removals release their reservations; live
subscriptions release theirs when disconnected. This also bounds control
backlog when a worker is slow. Disconnect can enqueue its terminal event
without competing for another slot; repeated connect/disconnect cannot bypass
the source limit.

The active-stream limit includes disconnected subscriptions until their source
cleanup finishes. Each connection reserves its terminal cleanup capacity and
prepares the worker before starting its source. While cleanup holds the final
slot, a new connection returns `Overloaded`; an admitted subscription's
`DisconnectData` can always revoke its topic and transfer its cleanup. Queue
storage grows only when the actual number of live or retiring owners reaches
a new high-water mark; its configured maximum is not eagerly allocated.
Warm cleanup reservations and releases use an atomic quota, while enqueue
and dequeue retain the worker queue mutex.

## Backpressure and errors

`RtdFifo::new(capacity)` queues accepted data in order up to the
per-subscription capacity. `RtdLatest` keeps at most one pending
data value, replacing it with newer accepted data. It does not retract a
publication already in flight. Both modes also enforce source and runtime
buffer budgets. A rejected replacement preserves the old pending value and
returns the new value in `RtdSendError::Full`. Terminal errors remain separate
from data and cannot be replaced by later samples. Select the blocking adapter's policy with
`RtdChannelSource::with_policy(RtdLatest, factory)` or keep the FIFO
`RtdChannelSource::new(capacity, factory)` constructor. Multiplexed sources take
the policy as their first constructor argument. Senders and multiplex contexts
carry the policy type (`RtdSender<T, RtdLatest>` and
`RtdSourceContext<T, RtdLatest>`); default type parameters select FIFO.
`delivery()` returns the `RtdDelivery` description for inspection.

`RtdSender::try_send` validates values before enqueueing and never waits for
queue capacity. `RtdSendError::Full` and `RtdSendError::Closed` retain the
validated update as `RtdPendingValue<T>`. Recover it with `into_pending()` and retry
on any `RtdSender<T, D>` of the same input type, including a different policy, with `try_send_pending()`, without
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
shutdown when it follows cancellation. Returning `Closing` from a producer
before cancellation is diagnosed as a producer failure.

`RtdSender::try_send_batch` converts and validates at most 32 inputs before
queue admission. FIFO accepts the whole batch in order or accepts none, with
one aggregate count/byte reservation against each shared budget. Latest validates
every input but atomically coalesces a successful batch to its final value;
it never publishes a prefix. A rejected batch leaves the previous latest value
intact.

`RtdBatchSendError::Full` and `Closed` retain all validated values as
`RtdPendingBatch<T>`. Use `into_pending()` and `try_send_pending_batch()` to retry
on any sender of the same input type, without converting again. This includes
retrying a rejected latest batch on a FIFO sender. `Invalid` discards the
converted prefix and remaining inputs without changing the queue. More than
32 inputs is invalid. Conversion occurs even when closed; an empty batch is a
successful no-op, including on a closed sender. The publisher still handles at
most 32 values per turn, so batch admission does not change inter-topic fairness
or promise that Excel displays every intermediate sample.

Producer errors and panics are diagnosed at completion. Publication or
worker-join failures can also be returned by shutdown. The framework revokes
each topic's publication capability before its subscription is reclaimed;
source-wide shutdown joins shared workers.

Publishing validates and queues a value; xlfn notifies Excel and handles
`RefreshData`. On Windows, a dedicated worker coalesces notices so a blocked
`UpdateNotify` COM call does not block the publisher from updating other topics.
An outstanding notice is retried 250 ms after the previous call finishes,
including when Excel accepted the call but has not fetched the data. A successful
`RefreshData` acknowledges the notice; a failed export keeps it pending. A newer
notice queued during the refresh is preserved. The worker sleeps while idle and
is joined before the callback is released at server shutdown. This mechanism
does not guarantee an Excel recalculation deadline while Excel is busy.

## Stop a producer

During disconnect, the channel stops accepting values, discards queued
updates, waits for in-flight publication, and revokes its sink. A blocking
subscription then joins its producer; a multiplexed subscription enqueues
`Unsubscribe` and leaves its shared source worker serving other topics.
Excel's `DisconnectData` first revokes the topic, then
transfers this cleanup to the generation-owned worker so Excel's thread can
service an outstanding COM callback. The Windows cleanup worker initializes
COM as an MTA. Sender cancellation follows when the
worker reaches this subscription; `DisconnectData` does not wait for its
producer to exit. Live subscriptions and queued or running cleanups together
are bounded by the runtime's active-stream limit; a stalled cleanup delays later
cleanups. Runtime close requests every shared source to stop, finishes
subscription cleanup, and joins shared source workers before stopping the
publisher or reclaiming sources. A source context's `recv_timeout` wakes with
`XllError::Closing` after stop. Shared jobs must observe cancellation and return;
arbitrarily blocked user I/O cannot be forcibly terminated safely.
Cleanup failures are diagnosed and retained for shutdown. The
`RtdSubscription::disconnect_and_wait` lifetime barrier remains synchronous.
Sender clones may outlive the
subscription, but valid sends return `RtdSendError::Closed`; conversion errors
can still return `RtdSendError::Invalid`. They retain no live RTD capability or
queued payloads. Producer captures belong to that job and are dropped before
its worker is joined. Dropping a multiplexed source also requests stop and
joins its worker as a fallback.

A producer that finishes successfully closes sender admission and drains
accepted values. The shared publisher remains available to other topics.
Ensure producer workers exit promptly when the sender closes or becomes cancelled.
Producer and custom cleanup code must not synchronously initiate this add-in's
own removal through Excel: removal waits for those same workers to stop.

## Custom sources

`RtdMultiplexedSource` and `RtdChannelSource` keep sink lifetime management
inside the framework. The unsafe `RtdSource` and `RtdSubscription` traits
remain available for integrations requiring direct publication. Read their
complete safety contracts before implementing them, including error and
panic paths. Source-wide `request_stop` and `shutdown_and_wait` hooks are
separate from per-topic cancellation and disconnection.

`RtdSubscription::disconnect_and_wait(self)` consumes the concrete subscription
by value. The add-in selects its owner type with `Addin::RtdSubscription`.
Each connection moves this value through rollback, the server, and deferred
cleanup without a per-connection box or source-storage mutex. Cancellation
borrows the owner; the consuming disconnect completes the sink-user barrier
before releasing cleanup capacity, including errors and panics. The runtime
keeps the source and publish core alive throughout that barrier.

Use the source's concrete subscription type for an add-in with one kind of
source. For several kinds, declare their closed set and use it as the associated
type:

```rust
xlfn::rtd_subscriptions! {
    /// Concrete owners accepted by this add-in.
    pub enum Subscriptions {
        Blocking(xlfn::rtd::RtdChannelSubscription),
        Shared(xlfn::rtd::RtdMultiplexedSubscription<f64>),
    }
}
// In `impl Addin`: type RtdSubscription = Subscriptions;
// Also select RtdSources: a tuple of named source types, or
// xlfn::DynamicRtdSources<Self::RtdSubscription> for incremental registration.
```

The macro generates direct enum construction and delegates each barrier to
its concrete variant. A custom conversion must implement the unsafe
`IntoRtdSubscription<E>` contract; an ordinary `From` implementation is not
sufficient because it could lose or retain a separate sink owner, or panic
before the barrier. Omit both RTD associated types for an add-in with no
registered RTD sources.

An enum occupies enough space for its largest variant. This affects server
entries and the cleanup queue, so keep unusually large producer data in the
source or a producer-owned allocation and measure bytes as well as allocations.

## Temporary COM registration

The XLL registers its RTD COM server temporarily. On a later start after an
abnormal Excel exit, xlfn cleans up stale registrations belonging to the same
XLL.

Installation still needs appropriate user-profile registry access. Test start, normal close, forced Excel termination, and restart in the deployment environment.

See [Deployment](deployment.md) for installation. Authenticate external feeds
in the application that supplies RTD data.
