//! Owned cell encoding handed from call dispatch to ABI publication.

use crate::XllResult;
use crate::return_abi::{XlArrayOutput, XlScalarOutput};
#[cfg(test)]
use crate::value::IntoExcel;

/// A fully encoded worksheet return, before its root is placed in an
/// Excel-owned `XLOPER12` return block.
#[doc(hidden)]
pub enum ReturnPayload {
    Scalar(XlScalarOutput),
    Array(XlArrayOutput),
}

#[cfg(test)]
impl ReturnPayload {
    #[inline]
    pub(crate) fn scalar(value: impl IntoExcel) -> XllResult<Self> {
        XlScalarOutput::encode(value).map(Self::Scalar)
    }
}

/// Concrete return encoding retained until ABI publication.
#[doc(hidden)]
pub trait EncodedReturn: Sized {
    fn prepare(self) -> XllResult<crate::return_abi::PreparedReturn>;
}
impl EncodedReturn for XlScalarOutput {
    fn prepare(self) -> XllResult<crate::return_abi::PreparedReturn> {
        Ok(crate::return_abi::PreparedReturn::from_scalar_output(self))
    }
}
impl EncodedReturn for XlArrayOutput {
    fn prepare(self) -> XllResult<crate::return_abi::PreparedReturn> {
        crate::return_abi::PreparedReturn::from_array_output(self)
    }
}
impl EncodedReturn for ReturnPayload {
    fn prepare(self) -> XllResult<crate::return_abi::PreparedReturn> {
        match self {
            Self::Scalar(value) => value.prepare(),
            Self::Array(value) => value.prepare(),
        }
    }
}
