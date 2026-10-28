//! Return dispatch implementations for rectangular collections.

use super::ReturnContext;
use crate::XllResult;
use crate::return_abi::XlArrayBuilder;
use crate::value::{Column, IntoExcel, Matrix, PlainInputMode, Row};

impl crate::call_return::ExcelReturnSealed for crate::return_abi::XlArrayOutput {}

impl crate::call_return::ExcelReturn for crate::return_abi::XlArrayOutput {
    type InputMode = PlainInputMode;
    type Payload = crate::return_abi::XlArrayOutput;

    fn into_excel<S: crate::RtdSubscription, C: crate::RtdSourceCatalog<S>>(
        self,
        _: &mut ReturnContext<'_, '_, Self::InputMode, S, C>,
    ) -> XllResult<Self::Payload> {
        Ok(self)
    }
}

impl crate::call_return::MainThreadReturn for crate::return_abi::XlArrayOutput {}
impl crate::call_return::ThreadSafeReturn for crate::return_abi::XlArrayOutput {}
impl crate::call_return::MacroSheetReturn for crate::return_abi::XlArrayOutput {}
impl crate::call_return::AsyncReturn for crate::return_abi::XlArrayOutput {}
impl crate::call_return::VolatileReturn for crate::return_abi::XlArrayOutput {}

impl<T: IntoExcel> crate::call_return::ExcelReturnSealed for Matrix<T> {}

impl<T: IntoExcel> crate::call_return::ExcelReturn for Matrix<T> {
    type InputMode = PlainInputMode;
    type Payload = crate::return_abi::XlArrayOutput;

    fn into_excel<S: crate::RtdSubscription, C: crate::RtdSourceCatalog<S>>(
        self,
        _: &mut ReturnContext<'_, '_, Self::InputMode, S, C>,
    ) -> XllResult<Self::Payload> {
        let rows = self.rows();
        let columns = self.columns();
        let mut builder = XlArrayBuilder::new(rows, columns)?;
        for value in self.into_vec() {
            builder.push(value)?;
        }
        builder.finish()
    }
}

impl<T: IntoExcel> crate::call_return::MainThreadReturn for Matrix<T> {}
impl<T: IntoExcel> crate::call_return::ThreadSafeReturn for Matrix<T> {}
impl<T: IntoExcel> crate::call_return::MacroSheetReturn for Matrix<T> {}
impl<T: IntoExcel> crate::call_return::AsyncReturn for Matrix<T> {}
impl<T: IntoExcel> crate::call_return::VolatileReturn for Matrix<T> {}

impl<T: IntoExcel> crate::call_return::ExcelReturnSealed for Row<T> {}

impl<T: IntoExcel> crate::call_return::ExcelReturn for Row<T> {
    type InputMode = PlainInputMode;
    type Payload = crate::return_abi::XlArrayOutput;

    fn into_excel<S: crate::RtdSubscription, C: crate::RtdSourceCatalog<S>>(
        self,
        _: &mut ReturnContext<'_, '_, Self::InputMode, S, C>,
    ) -> XllResult<Self::Payload> {
        let mut builder = XlArrayBuilder::new(1, self.as_slice().len())?;
        for value in self.into_vec() {
            builder.push(value)?;
        }
        builder.finish()
    }
}

impl<T: IntoExcel> crate::call_return::MainThreadReturn for Row<T> {}
impl<T: IntoExcel> crate::call_return::ThreadSafeReturn for Row<T> {}
impl<T: IntoExcel> crate::call_return::MacroSheetReturn for Row<T> {}
impl<T: IntoExcel> crate::call_return::AsyncReturn for Row<T> {}
impl<T: IntoExcel> crate::call_return::VolatileReturn for Row<T> {}

impl<T: IntoExcel> crate::call_return::ExcelReturnSealed for Column<T> {}

impl<T: IntoExcel> crate::call_return::ExcelReturn for Column<T> {
    type InputMode = PlainInputMode;
    type Payload = crate::return_abi::XlArrayOutput;

    fn into_excel<S: crate::RtdSubscription, C: crate::RtdSourceCatalog<S>>(
        self,
        _: &mut ReturnContext<'_, '_, Self::InputMode, S, C>,
    ) -> XllResult<Self::Payload> {
        let mut builder = XlArrayBuilder::new(self.as_slice().len(), 1)?;
        for value in self.into_vec() {
            builder.push(value)?;
        }
        builder.finish()
    }
}

impl<T: IntoExcel> crate::call_return::MainThreadReturn for Column<T> {}
impl<T: IntoExcel> crate::call_return::ThreadSafeReturn for Column<T> {}
impl<T: IntoExcel> crate::call_return::MacroSheetReturn for Column<T> {}
impl<T: IntoExcel> crate::call_return::AsyncReturn for Column<T> {}
impl<T: IntoExcel> crate::call_return::VolatileReturn for Column<T> {}
