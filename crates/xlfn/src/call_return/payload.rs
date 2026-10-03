//! Owned cell encoding handed from call dispatch to ABI publication.

use crate::XllResult;
use crate::return_abi::{XlArrayOutput, XlScalarOutput};
use crate::value::IntoExcel;

/// A fully encoded worksheet return, before its root is placed in an
/// Excel-owned `XLOPER12` return block.
#[doc(hidden)]
pub enum ReturnPayload {
    Scalar(XlScalarOutput),
    Array(XlArrayOutput),
}

impl ReturnPayload {
    #[inline]
    pub(crate) fn scalar(value: impl IntoExcel) -> XllResult<Self> {
        XlScalarOutput::encode(value).map(Self::Scalar)
    }
}
