# Worksheet functions

Add functions to the project from [Create your first add-in](quick-start.md).
This chapter covers ordinary inputs, return values, and errors; use
[Values and arrays](values.md) when a function needs ranges or mixed cell types.

## Expose a Rust calculation

Apply `#[excel_function]` to a safe, non-generic free function:

```rust
{{#include ../fixtures/addin.md}}
use xlfn::prelude::*;

#[excel_function(name = "MATH.HYPOT", thread_safe)]
pub fn hypot(x: f64, y: f64) -> f64 {
    x.hypot(y)
}
```

After rebuilding and loading the add-in, `=MATH.HYPOT(3, 4)` returns `5`.
Excel supplies `x` and `y`; xlfn converts them to finite Rust numbers before
calling the function. Text such as `"3"` is rejected rather than silently
converted. An input Excel error is propagated to the result.

The Excel-visible name can differ from the Rust function name. Choose a project
prefix for distributed functions, such as `ACME.MATH.HYPOT`, to avoid collisions
with other add-ins. Registration rejects conflicting names.

## Return an expected error

Use `XllResult<T>` when the calculation can fail. It is an alias for
`Result<T, XllError>`:

```rust
{{#include ../fixtures/addin.md}}
use xlfn::error::DomainErrorCode;
use xlfn::prelude::*;

#[excel_function(name = "MATH.SQRT", thread_safe)]
pub fn square_root(value: f64) -> XllResult<f64> {
    if value < 0.0 {
        return Err(XllError::Domain {
            code: DomainErrorCode::InvalidInput,
        });
    }
    Ok(value.sqrt())
}
```

`=MATH.SQRT(9)` returns `3`; a negative input returns `#NUM!`. This distinguishes
an invalid mathematical domain from a value of the wrong type. Use errors for
expected invalid input, and reserve panics for defects in the program.

You can return strings, booleans, numbers, and arrays, or wrap those results in
`XllResult`. If an Excel error is itself the intended value, return
`ExcelErrorValue`, for example `ExcelErrorValue(ExcelError::NotAvailable)` for
`#N/A`. [Errors and diagnostics](errors-diagnostics.md) explains the mappings and
how to record detailed failures.

## Choose when Excel may run it

The examples above use `thread_safe` because their entire calculation can run
concurrently. Use that flag only when every function, shared resource, and
external library on the call path supports concurrent access. It does not add
synchronization for you.

Without a mode flag or context, a synchronous function runs on Excel's main
thread. Functions that read shared state use an injected context as their first
parameter; that context also selects their execution mode. See
[Execution modes and contexts](execution-modes.md) before adding state or Excel
callbacks. A function that performs asynchronous work uses `async fn` and the
`async` Cargo feature; see [Asynchronous functions](async-functions.md).

## Function metadata

Rust doc comments on `#[excel_function]` automatically populate Excel's
Function Wizard description. You can also specify metadata explicitly via attributes:

```rust
{{#include ../fixtures/addin.md}}
# use xlfn::prelude::*;
/// Adds two numbers together.
#[excel_function(
    name = "MATH.ADD",
    category = "Math",
    description = "Calculates the sum of two numbers",
    thread_safe
)]
pub fn add(left: f64, right: f64) -> f64 {
    left + right
}
```

Next, see [Values and arrays](values.md) for accepting ranges and returning grids.
