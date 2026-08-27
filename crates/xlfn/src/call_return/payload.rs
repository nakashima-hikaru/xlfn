//! Semantic payload handed from call dispatch to the ABI encoder.

use crate::return_abi::XlArrayOutput;
use crate::value::ExcelCellOutput;

/// A fully converted worksheet return, before it is encoded into an
/// Excel-owned `XLOPER12` return block.
#[doc(hidden)]
pub enum ReturnPayload {
    Scalar(ExcelCellOutput),
    Array(XlArrayOutput),
}
