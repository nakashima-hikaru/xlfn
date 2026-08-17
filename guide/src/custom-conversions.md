# Custom conversions

The framework's conversion traits let domain types appear directly in worksheet signatures. Keep conversions strict, owned, bounded, and independent of Excel callbacks.

## Custom input with `FromExcel`

A custom input receives a call-scoped `XlValueRef` and the static argument name:

```rust
use xlfn::{
    value::{FromExcel, XlValueRef},
    error::{InputError, XllError, XllResult},
};

#[derive(Clone, Copy)]
struct PositiveRate(f64);

impl<'call> FromExcel<'call> for PositiveRate {
    fn from_excel(
        value: XlValueRef<'call>,
        argument: &'static str,
    ) -> XllResult<Self> {
        let rate = <f64 as FromExcel>::from_excel(value, argument)?;
        if rate < 0.0 {
            return Err(XllError::input(argument, InputError::OutOfRange));
        }
        Ok(Self(rate))
    }
}
```

The call lifetime is explicit. Owned conversions work for every `'call`; borrowed framework types such as `XlArrayRef<'call>` preserve that exact lifetime. Generated wrappers create a fresh lifetime per call, so Excel-owned memory cannot escape the exported function.

Reuse built-in conversions where possible. They already validate malformed pointers, UTF-16, numeric exactness, errors, shape, and memory limits.

Every type used as an Excel-visible parameter implements one `FromExcel` contract. Runtime context and formula-fingerprint construction stay inside the framework boundary. Do not retain `XlValueRef` or any pointer derived from it in an owned result.

## Custom cell and output conversions

A value used as one cell in a returned matrix, or as a custom scalar return, implements `IntoExcel`:

```rust
use xlfn::{
    value::{ExcelCellOutput, IntoExcel},
    error::XllResult,
};

struct Percentage(f64);

impl IntoExcel for Percentage {
    fn into_excel(self) -> XllResult<ExcelCellOutput> {
        self.0.into_excel()
    }
}
```

The same implementation is used for scalar returns and matrix cells. Execution-mode capability checks are supplied by the framework's internal return dispatcher.

A simpler alternative is to convert inside the function and return a built-in value:

```rust
#[excel_function(name = "RATE.PERCENT", thread_safe)]
fn percent(rate: PositiveRate) -> f64 {
    rate.0 * 100.0
}
```

## Custom result errors

Application errors can remain domain-specific:

```rust
use xlfn::error::{InputError, IntoXllError, XllError};

#[derive(Debug)]
enum DataError {
    MissingPillar,
    InvalidInput,
}

impl IntoXllError for DataError {
    fn into_xll_error(self) -> XllError {
        match self {
            Self::MissingPillar => XllError::input(
                "dataset",
                InputError::Malformed("missing pillar"),
            ),
            Self::InvalidInput => XllError::Domain {
                code: xlfn::error::DomainErrorCode::InvalidInput,
            },
        }
    }
}
```

Use `Result<T, DataError>` in the worksheet function. The generated boundary performs the conversion and records diagnostic detail.

Do not encode expected user errors as panics. Panic containment protects Excel from unwinding across the ABI, but it reports an internal defect rather than a domain error.

## Conversion design rules

A production conversion should satisfy all of these:

- **Owned:** no Excel pointer escapes the current call.
- **Bounded:** reject unreasonable strings, arrays, recursion, or allocation sizes.
- **Strict:** do not perform surprising text, locale, or Boolean coercions.
- **Deterministic:** the same cell value and configuration produce the same domain value.
- **Context-light:** conversion should not perform network calls or long-running external work.
- **Diagnostic:** preserve the argument name and use a meaningful `InputError` or domain code.

For a closed string vocabulary, prefer `ExcelEnum`. For a formula-owned object, use [handles](handles.md) rather than serializing an internal pointer into a string yourself.
