//! Worksheet-output conversion into semantic cell values.

use super::ExcelCellOutput;
use crate::error::DomainErrorCode;
use crate::{XllError, XllResult};

/// One numerical-output policy for semantic conversion and ABI encoding.
/// Invalid results are domain errors, regardless of the return shape or fast
/// path used to write them; input argument labels do not belong to this error.
pub(crate) fn validate_number(value: f64) -> XllResult<f64> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(XllError::Domain {
            code: DomainErrorCode::InvalidInput,
        })
    }
}

/// A destination that validates and writes one semantic Excel cell.
///
/// The value layer only knows this small semantic sink. ABI-specific array
/// builders implement it in the return ABI layer, so value conversion does
/// not depend on XLOPER12 allocation or return ownership.
#[doc(hidden)]
pub trait ExcelCellSink {
    fn push_cell(&mut self, value: ExcelCellOutput) -> XllResult<()>;
    fn push_f64(&mut self, value: f64) -> XllResult<()>;
    fn push_bool(&mut self, value: bool) -> XllResult<()>;
    fn push_str(&mut self, value: &str) -> XllResult<()>;
    fn push_string(&mut self, value: String) -> XllResult<()>;
    fn push_error(&mut self, value: crate::ExcelError) -> XllResult<()>;
}

/// Converts an ordinary Rust value into a semantic Excel cell.
#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot be converted into an Excel return value",
    label = "`{Self}` does not implement `IntoExcel`",
    note = "implement `IntoExcel` for `{Self}` or return a supported type (e.g. `f64`, `bool`, `String`, `ExcelErrorValue`, or a custom handle)"
)]
pub trait IntoExcel {
    fn into_excel(self) -> XllResult<ExcelCellOutput>;

    /// Writes directly into a semantic cell sink when the value has a
    /// primitive representation. Custom conversions keep the semantic
    /// fallback and never need to know the sink's ABI.
    /// Implementations must write exactly one cell with the same semantic
    /// value as `into_excel`, propagating conversion and sink errors. Scalar
    /// and array returns use this writer; borrowed text is encoded before its
    /// source is released.
    #[doc(hidden)]
    fn write_into<S: ExcelCellSink>(self, sink: &mut S) -> XllResult<()>
    where
        Self: Sized,
    {
        sink.push_cell(self.into_excel()?)
    }
}
