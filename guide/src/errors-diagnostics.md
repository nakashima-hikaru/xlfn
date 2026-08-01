# Errors and diagnostics

xlfn separates what Excel sees from what operators need to diagnose. A worksheet receives a conventional Excel error; the runtime can emit structured detail without exposing sensitive internals in the cell.

## Error types

Most add-ins use `XllResult<T>`, an alias for `Result<T, XllError>`.

Important `XllError` families include:

- `Input` with an argument name and `InputError`;
- `ExcelValue`, preserving an input Excel error;
- `Domain` with a stable `DomainErrorCode`;
- invalid or stale handles;
- lifecycle states such as closing or overloaded;
- native or callback failures represented by the relevant adapters;
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

This mapping is intentionally coarse. The diagnostic stream carries the specific variant, argument, function ID, and diagnostic identifier.

## Install the file sink

A basic production setup installs the built-in bounded file sink during `Addin::open`:

```rust
impl Addin for DeskTools {
    type State = State;
    type Error = XllError;

    fn open(_: &OpenContext) -> XllResult<State> {
        let path = xlfn::diagnostics::install_file_diagnostic_sink("desk-tools")
            .map_err(|_| XllError::Internal {
                diagnostic_id: 0x4449_4147_494e_4954,
            })?;
        tracing::info!(path = %path.display(), "diagnostic log installed");
        Ok(State::new())
    }
}
```

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

Install with `set_diagnostic_sink`. The runtime places a bounded asynchronous queue of 1,024 events in front of the sink. When producers outrun delivery, events are dropped rather than blocking worksheet execution. Monitor `dropped_diagnostic_events()` as an operational signal.

The sink itself must still be bounded and panic-free. A slow or reentrant sink can delay shutdown even though ordinary producers use a queue.

## `tracing` integration

The runtime emits structured `tracing` events in addition to the configured diagnostic sink. The host application owns the global subscriber. A library should not call `set_global_default` unconditionally; use an application-level subscriber policy that composes with other instrumentation.

Do not assume a tracing subscriber is infallible. The runtime contains panics around its own diagnostic boundaries, but add-in logging code should remain simple and non-panicking.

## Diagnostic IDs

Use stable, searchable IDs for internal failures:

```rust
const CONFIG_PARSE_FAILED: u64 = 0x434f_4e46_5041_5253;
```

Recommended practice:

1. define IDs as named constants near the subsystem;
2. never reuse an ID for a different failure;
3. include the ID in operator documentation and issue reports;
4. attach safe context such as function ID, package version, and path category;
5. avoid secrets, full workbook contents, access tokens, and raw customer data.

## User-facing error design

A high-quality worksheet API should make common errors actionable through function and argument descriptions. Diagnostics are not a substitute for clear contracts. Prefer:

- `#NUM!` for a mathematically invalid domain;
- `#N/A` for an unavailable or expired object/service result;
- a preserved upstream Excel error when it is semantically the input;
- `#VALUE!` for type or structure mismatch.

Use a companion information function only when users genuinely need structured status. Do not leak internal exception text into arbitrary worksheet cells.
