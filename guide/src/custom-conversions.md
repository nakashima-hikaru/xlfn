# Custom conversions

The framework's conversion traits let domain types appear directly in worksheet signatures. Keep conversions strict, owned, bounded, and independent of Excel callbacks.

## Custom input with `FromExcel`

A custom input receives a call-scoped `XlValueRef`, the static argument name, and a `CallContext`:

```rust
use xlfn::{
    convert::{CallContext, ExcelInputIdentity, FromExcel, InputIdentityEncoder, XlValueRef},
    error::{InputError, XllError, XllResult},
};

#[derive(Clone, Copy)]
struct PositiveRate(f64);

impl<'call> FromExcel<'call> for PositiveRate {
    fn from_excel(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &CallContext,
    ) -> XllResult<Self> {
        let rate = f64::from_excel(value, argument, context)?;
        if rate < 0.0 {
            return Err(XllError::input(argument, InputError::OutOfRange));
        }
        Ok(Self(rate))
    }
}

impl ExcelInputIdentity for PositiveRate {
    const IDENTITY_DOMAIN: &'static [u8] = b"example.positive-rate.v1";

    fn encode_identity(&self, encoder: &mut InputIdentityEncoder<'_>) {
        encoder.f64(self.0);
    }
}
```

The call lifetime is explicit. Owned conversions work for every `'call`; borrowed framework types such as `XlArrayRef<'call>` preserve that exact lifetime. Generated wrappers create a fresh lifetime per call, so Excel-owned memory cannot escape the exported function.

Reuse built-in conversions where possible. They already validate malformed pointers, UTF-16, numeric exactness, errors, shape, and memory limits.

Every type used as an Excel-visible parameter must implement both traits. `ExcelInputIdentity` is deliberately separate from conversion so a custom type can state its semantic equality explicitly. Do not hash a token, pointer, or source spelling unless that representation is part of the type's documented semantics. For a case-insensitive enum, hash the normalized variant; for a wrapper around a handle, hash the underlying `ObjectId` if the wrapper preserves handle identity.

`IDENTITY_DOMAIN` is the stable domain separator for the type. The framework writes it before calling `encode_identity`, so the method must encode only the value payload and must not write the type's top-level domain itself. Container implementations may write an element type's associated domain once before encoding the element stream.

## Custom output with `IntoExcelValue`

A value that converts to one ordinary Excel value implements `IntoExcelValue`:

```rust
use xlfn::{
    convert::{IntoExcelValue, OwnedExcelValue},
    error::XllResult,
};

struct Percentage(f64);

impl IntoExcelValue for Percentage {
    fn into_excel_value(self) -> XllResult<OwnedExcelValue> {
        self.0.into_excel_value()
    }
}
```

To return `Percentage` directly from an exported function, it also needs `ExcelReturn` and the marker traits for every permitted execution mode:

```rust
use xlfn::{
    convert::{ExcelReturn, MainThreadReturn, ReturnContext, ThreadSafeReturn},
    error::XllResult,
};

impl ExcelReturn for Percentage {
    type Output = Self;

    fn into_excel(
        self,
        _: &mut ReturnContext<'_, '_>,
    ) -> XllResult<Self::Output> {
        Ok(self)
    }
}

impl MainThreadReturn for Percentage {}
impl ThreadSafeReturn for Percentage {}
```

Implement only the mode markers you can justify. Marker traits are compile-time capability claims, not boilerplate to add indiscriminately.

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
