//! Generic RTD-value return dispatch.

use super::ReturnContext;
use crate::XllResult;
use crate::subscription::RtdValue;
use crate::value::{ExcelCellOutput, PlainInputMode};

impl super::ExcelReturnSealed for RtdValue {}

impl super::ExcelReturn for RtdValue {
    type InputMode = PlainInputMode;
    type Payload = crate::return_abi::XlScalarOutput;

    fn into_excel<S: crate::RtdSubscription, C: crate::RtdSourceCatalog<S>>(
        self,
        _: &mut ReturnContext<'_, '_, Self::InputMode, S, C>,
    ) -> XllResult<Self::Payload> {
        self.validate()?;
        let cell = match self {
            RtdValue::Number(value) => ExcelCellOutput::Number(value),
            RtdValue::Boolean(value) => ExcelCellOutput::Boolean(value),
            RtdValue::Integer(value) => ExcelCellOutput::Number(value as f64),
            RtdValue::String(value) => ExcelCellOutput::String(value),
            RtdValue::Error(value) => ExcelCellOutput::Error(value),
            RtdValue::Empty => ExcelCellOutput::Error(crate::ExcelError::NotAvailable),
        };
        crate::return_abi::XlScalarOutput::encode(cell)
    }
}

impl super::MainThreadReturn for RtdValue {}
impl super::ThreadSafeReturn for RtdValue {}
impl super::MacroSheetReturn for RtdValue {}
impl super::AsyncReturn for RtdValue {}
impl super::VolatileReturn for RtdValue {}
