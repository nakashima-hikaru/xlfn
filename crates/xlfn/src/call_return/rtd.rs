//! Generic RTD-value return dispatch.

use super::{ReturnContext, ReturnPayload};
use crate::XllResult;
use crate::subscription::RtdValue;
use crate::value::{ExcelCellOutput, PlainInputMode};

impl super::ExcelReturnSealed for RtdValue {}

impl super::ExcelReturn for RtdValue {
    type InputMode = PlainInputMode;

    fn into_excel(self, _: &mut ReturnContext<'_, '_>) -> XllResult<ReturnPayload> {
        self.validate()?;
        let cell = match self {
            RtdValue::Number(value) => ExcelCellOutput::Number(value),
            RtdValue::Boolean(value) => ExcelCellOutput::Boolean(value),
            RtdValue::Integer(value) => ExcelCellOutput::Number(value as f64),
            RtdValue::String(value) => ExcelCellOutput::String(value),
            RtdValue::Error(value) => ExcelCellOutput::Error(value.0),
            RtdValue::Empty => ExcelCellOutput::Error(crate::ExcelError::NotAvailable),
        };
        Ok(ReturnPayload::Scalar(cell))
    }
}

impl super::MainThreadReturn for RtdValue {}
impl super::ThreadSafeReturn for RtdValue {}
impl super::MacroSheetReturn for RtdValue {}
impl super::AsyncReturn for RtdValue {}
impl super::VolatileReturn for RtdValue {}
