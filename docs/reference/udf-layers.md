# UDF execution layers

Use an execution layer for bounded admission policy or telemetry that applies
to every exported UDF. No optional feature is required. Start with a working
[add-in and shared state](../../guide/src/lifecycle.md); use a function-local check when policy
depends on converted arguments. For choosing runtime facilities, see
[Choosing a pattern](../../guide/src/choosing-pattern.md).

The runtime already emits a standard structured completion event. Add a layer
only for policy or telemetry that event does not provide.

## Reject work before argument conversion

This layer applies a coarse concurrent-call limit:

```rust
use xlfn::execution::{CallMetadata, CallOutcome, UdfLayer, UdfLayerGuard};
use xlfn::{XllError, XllResult};

struct ConcurrencyLimit {
    maximum: usize,
}

struct NoopGuard;

impl UdfLayerGuard for NoopGuard {
    fn exit(self, _: &CallOutcome<'_>) {}
}

impl UdfLayer for ConcurrencyLimit {
    type Guard = NoopGuard;

    fn enter(&self, metadata: &CallMetadata) -> XllResult<Self::Guard> {
        if metadata.concurrent_calls > self.maximum {
            return Err(XllError::Overloaded);
        }
        Ok(NoopGuard)
    }
}
```

`enter` runs before argument conversion and cannot inspect typed arguments.
The concurrent count is a current observation, suitable for telemetry or
coarse admission decisions. Use application-owned permits for exact resource
accounting rather than treating this count as a reserved slot.

## Install a layer during open

Set the add-in's `Layers` type and return a tuple with the same types:

```rust
impl Addin for AppTools {
    type SharedState = State;
    type LifecycleState = ();
    type Error = XllError;
    type Layers = (ConcurrencyLimit,);

    fn open(_: &OpenContext) -> XllResult<Opened<State, (), Self::Layers>> {
        Ok(Opened::new(State::new())
            .with_layers((ConcurrencyLimit { maximum: 32 },)))
    }
}
```

Import the lifecycle types from `xlfn::prelude::*`. For no layers, use
`type Layers = ();` and return `Opened::new(state)`.

A tuple can contain up to 16 layers. Layers enter from left to right and exit
from right to left, like nested middleware. Each successful `enter` returns
its own guard for the call.

## Record metadata and outcomes

`CallMetadata` provides the stable UDF ID, Excel-visible name, runtime call ID,
calculation ID, start time, and current concurrent-call count. The calculation
ID correlates events in this runtime; do not persist it as workbook identity.

A guard's `exit` receives `CallOutcome` with framework-measured duration and
two independent classifications:

| Field | Meaning |
| --- | --- |
| `completion` | Computation succeeded, failed with a classified error, or was cancelled |
| `delivery` | Delivery was not applicable, succeeded, failed, or was unobserved |

For example, an async computation can succeed while Excel rejects its
`xlAsyncReturn`. That reports `UdfCompletionOutcome::Success` together with
`UdfDeliveryOutcome::Failed { .. }`. A computation error and delivery error
can both be retained in the same outcome.

The following guard illustrates copying bounded observations into your own
telemetry facility:

```rust
struct MetricsGuard {
    udf_id: &'static str,
}

impl UdfLayerGuard for MetricsGuard {
    fn exit(self, outcome: &CallOutcome<'_>) {
        // Record or enqueue a bounded classification and the measured duration.
        // Do not retain outcome or its borrowed error references.
        let _ = (self.udf_id, outcome.duration, &outcome.completion, &outcome.delivery);
    }
}
```

Errors inside an outcome are borrowed only for `exit`. Copy a bounded
classification or stable code if another task will report it later. Keep
queue capacity and overflow behavior explicit.

## Handle rejection and failure

If `enter` returns an error, already-entered guards exit in reverse order
with that classified error. A panic in `enter` becomes a panic error. Panics
in `exit` are contained so one observer does not prevent later guards from
exiting or unwind through the Excel ABI. If a call guard must be dropped
without ordinary completion, it reports an internal-error outcome.

Containment does not make complex instrumentation safe. Layer code runs on
every UDF path and must be small, bounded, and non-reentrant.

## Use layers for call-wide policy

Good uses include concurrency policy, latency and result metrics, trace
correlation, maintenance-mode rejection, bounded license checks, and external
adapter error-code distributions.

Do not mutate arguments or results, transform business rules, call Excel,
perform unbounded network logging, wait on long lock acquisitions, or create
one thread or task per invocation. Keep input-dependent business policy in
the worksheet function after conversion.
