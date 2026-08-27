//! Worksheet-output conversion into semantic cell values.

use super::ExcelCellOutput;
use crate::XllResult;

/// A destination for one already validated semantic Excel cell.
///
/// The value layer only knows this small semantic sink. ABI-specific array
/// builders implement it in the return ABI layer, so value conversion does
/// not depend on XLOPER12 allocation or return ownership.
#[doc(hidden)]
pub trait ExcelCellSink {
    fn push_cell(&mut self, value: ExcelCellOutput) -> XllResult<()>;
    fn push_f64(&mut self, value: f64) -> XllResult<()>;
    fn push_bool(&mut self, value: bool) -> XllResult<()>;
    fn push_string(&mut self, value: String) -> XllResult<()>;
    fn push_error(&mut self, value: crate::ExcelError) -> XllResult<()>;
}

/// Converts an ordinary Rust value into a semantic Excel cell.
#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot be converted into an Excel return value",
    label = "`{Self}` does not implement `IntoExcel`",
    note = "implement `IntoExcel` for `{Self}` or return a supported type (e.g. `f64`, `bool`, `String`, `ExcelError`, or a custom handle)"
)]
pub trait IntoExcel {
    fn into_excel(self) -> XllResult<ExcelCellOutput>;

    /// Writes directly into a semantic cell sink when the value has a
    /// primitive representation. Custom conversions keep the semantic
    /// fallback and never need to know the sink's ABI.
    #[doc(hidden)]
    fn write_into<S: ExcelCellSink>(self, sink: &mut S) -> XllResult<()>
    where
        Self: Sized,
    {
        sink.push_cell(self.into_excel()?)
    }
}
