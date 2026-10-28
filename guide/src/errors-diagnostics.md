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

`ExcelError`, `std::io::Error`, and `DiagnosticInitError` already implement
`IntoXllError` and convert into `XllError` with `From`, so they work with `?`.
For an intentional worksheet error, a function can return `Result<f64, ExcelError>`.
On the Q path, these values preserve the selected cell error without emitting a failure diagnostic.
Execution layers and UDF tracing still observe the `ExcelValue` completion:

```rust
{{#include ../fixtures/addin.md}}
use xlfn::prelude::*;

#[excel_function(name = "HELLO.AVAILABLE", thread_safe)]
fn available(value: f64) -> Result<f64, ExcelError> {
    if value < 0.0 {
        Err(ExcelError::NotAvailable)
    } else {
        Ok(value)
    }
}
```

When the same cell error needs application diagnostic detail, use
`XllError::custom(ExcelError::NotAvailable, "market data connection is unavailable")`.
On the Q path the cell receives `#N/A`; the message is retained in diagnostics.

## Error types

Most add-ins use `XllResult<T>`, an alias for `Result<T, XllError>`.

Important `XllError` families include:

- `Input` with an argument name and `InputError`;
- `ExcelValue`, preserving an input Excel error;
- `Domain` with a stable `DomainErrorCode`;
- `Custom` with an application-selected worksheet error and diagnostic text;
- invalid or stale handles;
- lifecycle states such as closing or overloaded;
- external-adapter or Excel-callback failures represented by the relevant application mappings;
- `Internal` with a stable diagnostic ID.

Use `XllError::input(argument, reason)` for user-correctable worksheet input. Reserve internal diagnostic IDs for defects or environmental failures that are not useful to expose directly in a cell.

## Worksheet mapping

The Q (`XLOPER12*`) path maps errors conservatively:

| Error family | Typical Excel result |
|---|---|
| domain errors and numeric overflow | `#NUM!` |
| preserved `ExcelError` and `Custom` | the selected Excel error |
| invalid/stale handle, closing, overloaded, or reentrant operation | `#N/A` |
| malformed input, wrong type, callback failure, or internal failure | `#VALUE!` |

Non-finite numeric outputs (`NaN` and either infinity) are
`DomainErrorCode::InvalidInput` and produce `#NUM!`. This applies equally to
scalar returns, `Matrix`/`Row`/`Column` cells, incremental `XlArrayBuilder`
output, and custom `IntoExcel` conversions. These output errors carry no
worksheet argument name. A non-finite numeric input remains
`InputError::NonFinite`, with the supplied argument name, and maps to `#VALUE!`.

Synchronous numeric arrays default to FP12 (`K%`). Every FP12 failure returns
null (`#NUM!`), including `ExcelValue` errors. Use
`#[excel_function(return_abi = "xloper12")]` to preserve Q error distinctions.
A diagnostic record cannot restore `IFNA` or other Excel formula semantics.
If Excel fails native argument coercion before calling Rust, xlfn receives no
call and cannot record that failure.

## Read bounded runtime history

History belongs to one add-in runtime, separately from the process-wide async
sink. Each detected synchronous failure is inserted before external delivery;
no sink or tracing subscriber is required. Querying history does not change
execution or shutdown state.

```rust
# fn read_history(diagnostics: &xlfn::diagnostics::DiagnosticHistory) {
use xlfn::diagnostics::DiagnosticCursor;
let mut cursor = DiagnosticCursor::START;
let page = diagnostics.errors_since(cursor, 100);
for error in &page.errors {
    println!("{}: {}", error.udf_id, error.message);
}
cursor = page.next_cursor;
let missed = page.missed_events;
# let _ = (cursor, missed);
# }
```

`ThreadSafeContext`, `MainThreadContext`, and `MacroSheetContext` expose
`diagnostics()` for application code. For example:

```rust
{{#include ../fixtures/addin.md}}
use xlfn::prelude::*;
use xlfn::diagnostics::DiagnosticCursor;

#[excel_function(name = "HELLO.ERROR.COUNT")]
fn error_count(context: ThreadSafeContext<'_, GuideAddin>) -> f64 {
    context.diagnostics().errors_since(DiagnosticCursor::START, 100).errors.len() as f64
}
```

Records include diagnostic, UDF, call,
and calculation identifiers, timestamp, error classification, the original
Excel error, argument, and bounded text. FP12 records retain the original
error even though the cell displays `#NUM!`. Calculation identity is captured
at UDF entry and advances on Excel's calculation-ended event in every feature
set. Excel can omit that event for some programmatic recalculations, so the ID
does not promise a distinct value for every recalculation request.
Standard panic text is retained;
arbitrary panic payloads are described without running their destructors.

Default retention is bounded by 1,024 records and 4 MiB of accounted record
storage. Old records are evicted. Pages expose `missed_events` for overwritten
or unrecordable events, and statistics expose cumulative drops. Allocation
failure can prevent recording or copying a page, so history is best effort.
Retain the returned cursor and inspect the page's allocation-failure flag before
retrying. Sink queue losses are counted separately.

## Install the file sink

A basic production setup installs the built-in bounded file sink during `Addin::open`:

```rust
{{#include ../fixtures/diagnostics.md}}
impl Addin for AppTools {
    type RtdSubscription = ();
    type RtdSources = xlfn::DynamicRtdSources<Self::RtdSubscription>;
    type SharedState = State;
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();
    #[cfg(feature = "async")]
    type AsyncExecutor = xlfn::NoAsyncExecutor;

    fn open(context: &OpenContext<Self>) -> XllResult<Opened<Self::SharedState, Self::LifecycleState, Self::Layers>> {
        let path = context.diagnostics().install_file_sink()?;
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

`LOCALAPPDATA` must be an absolute path. If it is missing, empty, or relative,
file-sink initialization fails; startup logging is skipped. There is no
temporary-directory fallback. The user profile is the filesystem trust boundary.
The file rotates at 4 MiB and retains three generations. Cross-process lock
contention fails the write immediately and increments the file-write failure
counter, allowing the diagnostic worker to continue processing later events.

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
    type RtdSubscription = ();
    type RtdSources = xlfn::DynamicRtdSources<Self::RtdSubscription>;
    type SharedState = State;
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();
    #[cfg(feature = "async")]
    type AsyncExecutor = xlfn::NoAsyncExecutor;

    fn open(context: &OpenContext<Self>) -> XllResult<Opened<Self::SharedState, Self::LifecycleState, Self::Layers>> {
        context.diagnostics().set_sink(Telemetry)?;
        Ok(Opened::new(State::new()))
    }
}
```

The runtime queues up to 1,024 events and limits additional cloned payloads to
16 MiB, including the event currently being delivered. Admitted events retain
their complete structured error. Oversize payloads, error chains deeper than
128 nodes, and exhausted budgets are dropped before cloning. Payload accounting
includes owned strings, paths, and source boxes; it excludes allocator overhead
and fixed queue storage. Monitor dropped events as an operational signal:

```rust
let dropped = xlfn::diagnostics::diagnostic_stats().dropped_events;
```

The sink itself must still be bounded and panic-free. A slow or reentrant sink can delay shutdown even though ordinary producers use a queue.

## `tracing` integration

The runtime emits structured `tracing` events in addition to the configured diagnostic sink. The host application owns the global subscriber. A library should not call `set_global_default` unconditionally; use an application-level subscriber policy that composes with other instrumentation.

The `error` and `error_debug` text fields are each bounded to 16 KiB plus a
truncation suffix. Ordinary text is unchanged. Subscribers run synchronously,
so their own formatting, delivery, and blocking policy still affect callers.

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
