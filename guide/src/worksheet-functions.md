# Worksheet functions

Apply `#[excel_function]` to a safe, ordinary, non-generic free function:

```rust
#[excel_function(name = "MATH.HYPOT", thread_safe)]
pub fn hypot(x: f64, y: f64) -> f64 {
    x.hypot(y)
}
```

The function may be synchronous or, with the `async` feature, an `async fn`. The macro rejects unsafe, `extern`, `const`, generic, variadic, and method declarations.

## Inputs and results are trait-driven

An ordinary input type implements `ExcelParameter`. This single contract converts the Excel representation into a Rust value and defines which converted value participates in formula revision identity. An ordinary output type participates in `ExcelReturn` and usually implements `IntoExcelValue`. The framework validates the selected execution mode at compile time through marker traits.

Application errors use `Result<T, E>` where `E: IntoXllError`:

```rust
#[derive(Debug)]
enum DataProcessingError {
    InvalidValue,
}

impl IntoXllError for DataProcessingError {
    fn into_xll_error(self) -> XllError {
        match self {
            Self::InvalidValue => XllError::input(
                "limit",
                xlfn::error::InputError::OutOfRange,
            ),
        }
    }
}

#[excel_function(name = "DATA.EVALUATE", thread_safe)]
fn intrinsic(factor: f64, beta: f64) -> Result<f64, DataProcessingError> {
    if limit < 0.0 {
        return Err(DataProcessingError::InvalidValue);
    }
    Ok((base - limit).max(0.0))
}
```

## Argument limits

- synchronous functions support at most 255 Excel-visible arguments;
- asynchronous functions support at most 254 because Excel supplies an additional async handle;
- the optional context argument is injected by the framework and does not count as an Excel-visible argument;
- argument patterns must be simple identifiers.

Large positional APIs are difficult to use even below these hard limits. Prefer domain objects, arrays, handles, or a small coherent worksheet surface.

## Thread safety is an explicit claim

Add `thread_safe` only when the full call path is safe under Excel multi-threaded recalculation:

```rust
#[excel_function(name = "DATASET.EVALUATE", thread_safe)]
fn evaluate(dataset: Handle<'_, Dataset>, time: f64) -> XllResult<f64> {
    dataset.evaluate(time)
}
```

This includes application state, external adapters, caches, logging, and destruction paths. The attribute is not a performance hint; it is a contract with Excel.

## Return ownership

Return values are converted into framework-owned XLOPER12 storage. Excel eventually calls the generated `xlAutoFree12` export. Do not allocate or free XLOPER12 values in ordinary add-in code.

The framework catches panics at its ABI boundaries and returns a safe Excel error while reporting the detailed failure. Panics remain bugs: containment protects Excel; it does not make a partially completed business operation transactional.

## Registration conflicts

Registration names are conflict-denying. xlfn does not replace another XLL's hidden or public name because it cannot safely reconstruct another add-in's ownership and visibility state. Choose stable, namespaced Excel names such as `ACME.DATA.COMPUTE`.
