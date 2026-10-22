//! Numeric-only return conversion, independent of Q cell construction.
use super::{ExcelReturn, ReturnContext, ReturnPayload};
use crate::XllResult;
use crate::error::IntoXllError;
use crate::return_abi::numeric::{NumericArrayBuilder, NumericArrayOutput};
use crate::value::{Column, Matrix, PlainInputMode, Row};

/// Framework dispatch for types supported by FP12 direct returns.
#[doc(hidden)]
#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot be returned using the FP12 ABI",
    note = "FP12 requires Matrix<f64>, Row<f64>, Column<f64>, NumericArrayOutput, or a Result containing one of these types"
)]
pub trait NumericReturn: ExcelReturn {
    fn into_numeric(self, context: &mut ReturnContext<'_, '_>) -> XllResult<NumericArrayOutput>;
    fn invoke(
        context: &mut ReturnContext<'_, '_>,
        operation: impl FnOnce() -> XllResult<Self>,
    ) -> XllResult<NumericArrayOutput> {
        operation()?.into_numeric(context)
    }
}

impl NumericReturn for Matrix<f64> {
    fn into_numeric(self, _: &mut ReturnContext<'_, '_>) -> XllResult<NumericArrayOutput> {
        NumericArrayBuilder::from_f64(self.rows(), self.columns(), self.as_slice())?.finish()
    }
}
impl NumericReturn for Row<f64> {
    fn into_numeric(self, _: &mut ReturnContext<'_, '_>) -> XllResult<NumericArrayOutput> {
        NumericArrayBuilder::from_f64(1, self.as_slice().len(), self.as_slice())?.finish()
    }
}
impl NumericReturn for Column<f64> {
    fn into_numeric(self, _: &mut ReturnContext<'_, '_>) -> XllResult<NumericArrayOutput> {
        NumericArrayBuilder::from_f64(self.as_slice().len(), 1, self.as_slice())?.finish()
    }
}
impl NumericReturn for NumericArrayOutput {
    fn into_numeric(self, _: &mut ReturnContext<'_, '_>) -> XllResult<NumericArrayOutput> {
        Ok(self)
    }
}
impl<T: NumericReturn, E: IntoXllError> NumericReturn for Result<T, E> {
    fn into_numeric(self, context: &mut ReturnContext<'_, '_>) -> XllResult<NumericArrayOutput> {
        self.map_err(IntoXllError::into_xll_error)?
            .into_numeric(context)
    }
}

impl super::ExcelReturnSealed for NumericArrayOutput {}
impl ExcelReturn for NumericArrayOutput {
    type InputMode = PlainInputMode;
    fn into_excel(self, _: &mut ReturnContext<'_, '_>) -> XllResult<ReturnPayload> {
        crate::return_abi::XlArrayBuilder::from_f64(self.rows(), self.columns(), self.as_slice())?
            .finish()
            .map(ReturnPayload::Array)
    }
}
impl super::MainThreadReturn for NumericArrayOutput {}
impl super::ThreadSafeReturn for NumericArrayOutput {}
impl super::MacroSheetReturn for NumericArrayOutput {}
impl super::AsyncReturn for NumericArrayOutput {}
impl super::VolatileReturn for NumericArrayOutput {}
