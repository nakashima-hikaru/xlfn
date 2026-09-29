# Errors and diagnostics

xlfn separates what Excel sees from what operators need to diagnose. A worksheet receives a conventional Excel error; the runtime can emit structured detail without exposing sensitive internals in the cell.

## Return a worksheet error

Use `XllResult<T>` when a calculation can fail. Add this function to the
[tutorial project](quick-start.md):

```rust
{{#include ../fixtures/addin.md}}
use xlfn::error::DomainErrorCode;
use xlfn::prelude::*;

#[excel_function(name = "HELLO.SQRT", thread_safe)]
fn square_root(value: f64) -> XllResult<f64> {
    if value < 0.0 {
        return Err(XllError::Domain {
            code: DomainErrorCode::InvalidInput,
        });
    }
    Ok(value.sqrt())
}
```

Return an error for an expected failure. If your application defines its own
error type, implement `IntoXllError` to convert it to `XllError`.

## Error types

Most add-ins use `XllResult<T>`, an alias for `Result<T, XllError>`.

Important `XllError` families include:

- `Input` with an argument name and `InputError`;
- `ExcelValue`, preserving an input Excel error;
- `Domain` with a stable `DomainErrorCode`;
- invalid or stale handles;
- lifecycle states such as closing or overloaded;
- external-adapter or Excel-callback failures represented by the relevant application mappings;
- `Internal` with a stable diagnostic ID.

Use `XllError::input(argument, reason)` for user-correctable worksheet input. Reserve internal diagnostic IDs for defects or environmental failures that are not useful to expose directly in a cell.

## Worksheet mapping

The framework maps errors conservatively:

| Error family | Typical Excel result |
|---|---|
| domain errors and numeric overflow | `#NUM!` |
| preserved `ExcelErrorValue` | the original Excel error |
| invalid/stale handle, closing, overloaded, or reentrant operation | `#N/A` |
| malformed input, wrong type, callback failure, or internal failure | `#VALUE!` |

Non-finite numeric outputs (`NaN` and either infinity) are
`DomainErrorCode::InvalidInput` and produce `#NUM!`. This applies equally to
scalar returns, `Matrix`/`Row`/`Column` cells, incremental `XlArrayBuilder`
output, and custom `IntoExcel` conversions. These output errors carry no
worksheet argument name. A non-finite numeric input remains
`InputError::NonFinite`, with the supplied argument name, and maps to `#VALUE!`.

This mapping is intentionally coarse. The diagnostic stream carries the specific variant, argument, function ID, and diagnostic identifier.

## Install the file sink

A basic production setup installs the built-in bounded file sink during `Addin::open`:

```rust
{{#include ../fixtures/diagnostics.md}}
impl Addin for AppTools {
    type SharedState = State;
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(context: &OpenContext) -> XllResult<Opened<Self::SharedState, Self::LifecycleState, Self::Layers>> {
        let path = context
            .diagnostics()
            .install_file_sink()
            .map_err(|error| XllError::Native {
                code: -1,
                message: error.to_string(),
            })?;
        tracing::info!(path = %path.display(), "diagnostic log installed");
        Ok(Opened::new(State::new()))
    }
}
```

The sink writes one JSON object per line. Control characters are escaped and
large text fields are bounded before writing, so an error cannot create
additional log records or bypass rotation.

The default path is:

```text
%LOCALAPPDATA%/<addin-id>/logs/diagnostics.log
```

When `LOCALAPPDATA` is unavailable, the implementation uses a temporary-directory fallback. The file rotates at 4 MiB and retains three generations.

The sink is process-wide. Installing a new sink flushes and joins the previous sink worker before replacement, and the framework stops the active sink during XLL shutdown. Make ownership explicit when multiple add-ins or test harnesses share a process; one add-in must not silently take telemetry ownership from another.

## Custom sinks

Implement `DiagnosticSink` when events must go to an existing telemetry system:

```rust
use xlfn::diagnostics::{DiagnosticEvent, DiagnosticSink};

struct Telemetry;

impl DiagnosticSink for Telemetry {
    fn report(&self, event: &DiagnosticEvent<'_>) {
        // Copy only the fields needed by the bounded downstream queue.
        // Do not block indefinitely or call Excel here.
        let _ = event;
    }
}
```

Install the custom sink during `Addin::open`:

```rust
{{#include ../fixtures/diagnostics.md}}
# use xlfn::diagnostics::{DiagnosticEvent, DiagnosticSink};
# struct Telemetry;
# impl DiagnosticSink for Telemetry { fn report(&self, _: &DiagnosticEvent<'_>) {} }
impl Addin for AppTools {
    type SharedState = State;
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(context: &OpenContext) -> XllResult<Opened<Self::SharedState, Self::LifecycleState, Self::Layers>> {
        context
            .diagnostics()
            .set_sink(Telemetry)
            .map_err(|error| XllError::Native {
                code: -1,
                message: error.to_string(),
            })?;
        Ok(Opened::new(State::new()))
    }
}
```

The runtime places a bounded asynchronous queue of 1,024 events in front of the sink. When producers outrun delivery, events are dropped rather than blocking worksheet execution. Monitor dropped events as an operational signal:

```rust
let dropped = xlfn::diagnostics::diagnostic_stats().dropped_events;
```

The sink itself must still be bounded and panic-free. A slow or reentrant sink can delay shutdown even though ordinary producers use a queue.

## `tracing` integration

The runtime emits structured `tracing` events in addition to the configured diagnostic sink. The host application owns the global subscriber. A library should not call `set_global_default` unconditionally; use an application-level subscriber policy that composes with other instrumentation.

Do not assume a tracing subscriber is infallible. The runtime contains panics around its own diagnostic boundaries, but add-in logging code should remain simple and non-panicking.

## Diagnostic IDs

The runtime attaches a `DiagnosticId` to internal failures and emitted diagnostic events. Sinks can inspect this identifier via `.as_u64()` or format it as hexadecimal:

```rust
# use xlfn::diagnostics::{DiagnosticEvent, DiagnosticSink};
# struct Telemetry;
impl DiagnosticSink for Telemetry {
    fn report(&self, event: &DiagnosticEvent<'_>) {
        let numeric_id = event.diagnostic_id().as_u64();
        tracing::error!(diagnostic = %format_args!("{numeric_id:016x}"), "UDF failure event");
    }
}
```

These IDs are framework-internal correlation codes rather than user-defined error types.

## User-facing error design

A high-quality worksheet API should make common errors actionable through function and argument descriptions. Diagnostics are not a substitute for clear contracts. Prefer:

- `#NUM!` for a mathematically invalid domain;
- `#N/A` for an unavailable or expired object/service result;
- a preserved upstream Excel error when it is semantically the input;
- `#VALUE!` for type or structure mismatch.

Use a companion information function only when users genuinely need structured status. Do not leak internal exception text into arbitrary worksheet cells.

## Next steps

- See [Troubleshooting](troubleshooting.md) for resolving specific Excel errors.
- See [Choose a calculation pattern](choosing-pattern.md) to explore stateful and asynchronous designs.
