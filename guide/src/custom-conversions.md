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
struct PositiveFactor(f64);

impl<'call> FromExcel<'call> for PositiveFactor {
    fn from_excel(
        value: XlValueRef<'call>,
        argument: &'static str,
    ) -> XllResult<Self> {
        let factor = <f64 as FromExcel>::from_excel(value, argument)?;
        if factor < 0.0 {
            return Err(XllError::input(argument, InputError::OutOfRange));
        }
        Ok(Self(factor))
    }
}
```

The call lifetime is explicit. Owned conversions work for every `'call`; borrowed framework types such as `XlArrayRef<'call>` preserve that exact lifetime. Generated wrappers create a fresh lifetime per call, so Excel-owned memory cannot escape the exported function.

Reuse built-in conversions where possible. They already validate malformed pointers, UTF-16, numeric exactness, errors, shape, and memory limits.

### Collection and optional inputs

Use `value::convert` to apply those same rules inside a custom collection
conversion. Each function accepts an element converter, such as
`f64::from_excel`, and preserves the argument name in conversion errors:

```rust
use xlfn::{value::{convert, FromExcel, Matrix, XlValueRef}, XllResult};

struct NumericMatrix(Matrix<f64>);

impl<'call> FromExcel<'call> for NumericMatrix {
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        let matrix = convert::matrix(value, argument, f64::from_excel)?;
        // Apply application-specific constraints to the validated matrix here.
        Ok(Self(matrix))
    }
}
```

`matrix`, `vector`, `row`, and `column` enforce the corresponding worksheet
shape. `bounded_var_args` also checks the maximum count before converting any
elements. These functions share the built-in array allocation limits and
reject nested arrays. `optional` maps omitted and blank values to `None`;
`optional_value` preserves their distinction with `OptionalExcelValue`.
Both accept another collection conversion as their converter:

```rust
let matrix: Option<Matrix<f64>> = convert::optional(value, argument, |value, argument| {
    convert::matrix(value, argument, f64::from_excel)
})?;
```

The element converter can be another custom `FromExcel` implementation. A
converter that returns a borrowed view retains the input's call lifetime;
the helper does not turn that view into an owned value. Custom handle-producer
inputs still encode their final semantic value with `ExcelInputIdentity`, as
described below.

Owned types used as ordinary Excel-visible parameters implement one `FromExcel`
contract. The framework also provides explicit call-scoped views such as
`&str`, `MatrixRef<'_, T>`, and `ExcelCellRef<'_>`; those are synchronous-only
and are not custom `FromExcel` implementations. Runtime context and
formula-fingerprint construction stay inside the framework boundary. Do not
retain `XlValueRef` or any pointer derived from it in an owned result.

### Semantic identity for handle producers

A function returning a formula-owned handle is memoized by the converted
semantic inputs. Such a parameter must therefore also implement
`ExcelInputIdentity`; ordinary functions that do not produce a handle still
require only `FromExcel`:

```rust
use xlfn::value::{ExcelInputIdentity, InputIdentityEncoder};

impl ExcelInputIdentity for PositiveFactor {
    fn encode_input_identity(&self, encoder: &mut InputIdentityEncoder) {
        encoder.f64(self.0);
    }
}
```

The identity implementation describes the Rust value observed by the UDF,
not the original `XLOPER12` representation. Omitting this implementation for
a custom input in a handle-producing function is a compile-time error. The
framework supplies semantic identities for built-in conversions and derives a
variant ordinal for `ExcelEnum` values.

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
#[excel_function(name = "FACTOR.PERCENT", thread_safe)]
fn percent(factor: PositiveFactor) -> f64 {
    factor.0 * 100.0
}
```

## Custom result errors

Application errors can remain domain-specific:

```rust
use xlfn::error::{InputError, IntoXllError, XllError};

#[derive(Debug)]
enum DataError {
    MissingEntry,
    InvalidInput,
}

impl IntoXllError for DataError {
    fn into_xll_error(self) -> XllError {
        match self {
            Self::MissingEntry => XllError::input(
                "dataset",
                InputError::Malformed("missing entry"),
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
