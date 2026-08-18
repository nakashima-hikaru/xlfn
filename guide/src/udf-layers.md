# UDF execution layers

Execution layers provide bounded admission control and instrumentation around every exported UDF. They observe call metadata before argument conversion and receive a classified outcome after completion.

Import from:

```rust
use xlfn::advanced::execution::{
    CallMetadata, CallOutcome, UdfLayer, UdfLayerGuard, UdfResultKind,
};
```

## Implement a layer

```rust
use std::sync::Arc;
use std::time::Instant;

struct MetricsLayer;

struct MetricsGuard {
    udf_id: &'static str,
    started: Instant,
}

impl UdfLayer for MetricsLayer {
    fn enter(&self, metadata: &CallMetadata) -> XllResult<Box<dyn UdfLayerGuard>> {
        Ok(Box::new(MetricsGuard {
            udf_id: metadata.udf_id,
            started: Instant::now(),
        }))
    }
}

impl UdfLayerGuard for MetricsGuard {
    fn exit(self: Box<Self>, outcome: &CallOutcome<'_>) {
        tracing::info!(
            udf = self.udf_id,
            result = ?outcome.result,
            duration_ns = outcome.duration.as_nanos(),
            local_duration_ns = self.started.elapsed().as_nanos(),
            "instrumented UDF"
        );
    }
}
```

Register layers from the add-in:

```rust
impl Addin for DeskTools {
    type State = State;
    type Error = XllError;

    fn open(_: &OpenContext) -> XllResult<State> {
        Ok(State::new())
    }

    fn udf_layers(_: &State) -> Vec<Box<dyn UdfLayer>> {
        vec![Box::new(MetricsLayer)]
    }
}
```

The vector order is enter order. Guards exit in reverse order, like nested middleware.

## Metadata

`CallMetadata` includes:

- stable UDF ID;
- Excel-visible name;
- process-generation call ID;
- calculation ID;
- start time;
- current concurrent-call count.

The calculation ID is a runtime correlation identifier, not a workbook persistence key. The concurrent count is suitable for telemetry and coarse admission decisions, not exact resource accounting.

## Outcomes

`CallOutcome` contains:

- `UdfResultKind`: success, input, domain, vendor, panic, closing, or internal;
- an optional borrowed `XllError`;
- an optional vendor status code;
- framework-measured duration.

The borrowed error is valid only during `exit`. Copy a bounded classification or stable code into an asynchronous telemetry queue; do not retain the reference.

## Admission control

A layer can reject a call by returning an `XllError` from `enter`:

```rust
struct ConcurrencyLimit {
    maximum: usize,
}

struct NoopGuard;

impl UdfLayerGuard for NoopGuard {
    fn exit(self: Box<Self>, _: &CallOutcome<'_>) {}
}

impl UdfLayer for ConcurrencyLimit {
    fn enter(&self, metadata: &CallMetadata) -> XllResult<Box<dyn UdfLayerGuard>> {
        if metadata.concurrent_calls > self.maximum {
            return Err(XllError::Overloaded);
        }
        Ok(Box::new(NoopGuard))
    }
}
```

Admission runs before argument conversion. It therefore cannot inspect typed arguments. This is intentional: layers remain generic runtime policy rather than an alternate business-function mechanism.

Use a function-local check when policy depends on a input, resource, model, or other converted value.

## Failure behavior

- If a layer's `enter` returns an error, already-entered guards exit in reverse order with that classified error.
- A panic in `enter` is converted to a panic error.
- Panics in `exit` are caught so that one observer does not unwind through the ABI or prevent later guards from exiting.
- A dropped internal guard receives an internal-error outcome as a final safety path.

Containment does not justify fallible or complex instrumentation. Layer code is on every UDF path and must be small, bounded, and non-reentrant.

## Appropriate uses

Good uses include:

- concurrency limits;
- per-function latency and result metrics;
- trace correlation;
- maintenance-mode rejection;
- bounded license/admission checks that do not call Excel;
- recording external-adapter error-code distributions.

Avoid:

- mutating arguments or results;
- business-rule transformations;
- unbounded network logging;
- workbook callbacks;
- long lock acquisition;
- creating one thread or task per invocation.

The runtime already emits a standard structured completion event. Add a layer only for policy or telemetry that the built-in event does not provide.
