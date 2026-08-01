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

## Implement a source

```rust
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use xlfn::{
    error::InputError,
    prelude::*,
};

struct MetricSource {
    client: Arc<Client>,
}

impl RtdSource for MetricSource {
    type Value = RtdValue;

    fn subscribe(
        &self,
        topic: &RtdTopic,
        sink: RtdSink<Self::Value>,
    ) -> XllResult<Box<dyn RtdSubscription>> {
        let [kind, symbol] = topic.parts() else {
            return Err(XllError::input(
                "RTD topic",
                InputError::Malformed("expected [kind, symbol]"),
            ));
        };
        if kind != "last" {
            return Err(XllError::input(
                "RTD topic",
                InputError::Malformed("unsupported metric topic"),
            ));
        }
        let symbol = symbol.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let client = Arc::clone(&self.client);

        let worker = std::thread::spawn(move || {
            while !worker_cancelled.load(Ordering::Acquire) {
                match client.try_next_metric(&symbol) {
                    Ok(Some(value)) => {
                        if sink.publish(RtdValue::Number(value)).is_err() {
                            break;
                        }
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                    Err(_) => {
                        let _ = sink.publish(RtdValue::Error(ExcelErrorValue(
                            ExcelError::NotAvailable,
                        )));
                        break;
                    }
                }
            }
        });

        Ok(Box::new(MetricSubscription {
            cancelled,
            worker: Some(worker),
        }))
    }
}
```

The source's subscription path must be bounded. It may create a worker or register with an existing event loop, but it should not wait indefinitely for the first external message. In this illustrative loop, `try_next_metric` is a non-blocking poll; in production, prefer a cancellation-aware channel or event loop over periodic polling.

## Implement shutdown correctly

```rust
struct MetricSubscription {
    cancelled: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl RtdSubscription for MetricSubscription {
    fn request_cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    fn disconnect_and_wait(mut self: Box<Self>) -> XllResult<()> {
        self.request_cancel();
        if let Some(worker) = self.worker.take() {
            worker.join().map_err(|_| XllError::Internal {
                diagnostic_id: 0x5254_4457_4f52_4b52,
            })?;
        }
        Ok(())
    }
}
```

`request_cancel` must be non-blocking, idempotent, panic-free, and must not call Excel or re-enter framework subscription APIs. `disconnect_and_wait` performs the quiescence barrier: when it returns, the source must no longer execute XLL code or publish through the sink.

Do not implement a timeout that abandons an in-process callback and then permits the XLL to unload. Put uninterruptible producers in another process.

## Use the source from a function

Keep the source in add-in state and subscribe from a main-thread function:

```rust
#[excel_function(name = "METRIC.LAST")]
fn last_metric(
    #[excel_context(main_thread)] context: MainThreadContext<'_, State>,
    symbol: String,
) -> XllResult<RtdValue> {
    let topic = RtdTopic::new(["last", symbol.as_str()])?;
    context.subscribe(context.state().metrics.clone(), topic)
}
```

The source is typically an `Arc<S>` or another cloneable value satisfying the source API. The exact identity of source plus topic determines sharing. Multiple formulas that observe the same active subscription share it; a failed new observation rolls back only the reservation created by that attempt, not an unrelated established subscriber.

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

`RtdSink::publish` can fail when the runtime is closing, the subscription is no longer active, the value is invalid, or internal capacity is unavailable. A producer must handle the error and stop or retry with a bounded policy. Do not loop tightly on a permanent error.

Publishing validates and queues a value; notification and `RefreshData` happen through the framework. User code must never call the COM update event directly.

## Temporary COM registration

The XLL registers its RTD COM server temporarily. Ownership markers include the add-in identity, schema, module path, and CLSID. On a later start after an abnormal Excel exit, xlfn removes only stale registrations whose full marker set belongs to the same XLL. It does not broadly delete similarly named registry keys.

Installation still needs appropriate user-profile registry access. Test start, normal close, forced Excel termination, and restart in the deployment environment.

## Choosing RTD versus async

Use async for one eventual result. Use RTD for a value that may change repeatedly while a formula remains subscribed. Do not simulate streaming by starting an infinite async function; it prevents normal completion and complicates cancellation and unload.
