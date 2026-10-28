//! Formula-handle return dispatch.

use super::ReturnContext;
use crate::XllResult;
use crate::handle::ExcelHandleObject;
use crate::value::FormulaInputMode;

impl<T: ExcelHandleObject> super::ExcelReturnSealed for crate::handle::HandleAlias<'_, T> {}

impl<'call, T: ExcelHandleObject> super::ExcelReturn for crate::handle::HandleAlias<'call, T> {
    type InputMode = FormulaInputMode;
    type Payload = crate::return_abi::XlScalarOutput;

    fn into_excel<S: crate::RtdSubscription, C: crate::RtdSourceCatalog<S>>(
        self,
        context: &mut ReturnContext<'_, '_, Self::InputMode, S, C>,
    ) -> XllResult<Self::Payload> {
        context
            .publish_existing_alias(|| Ok(self))
            .and_then(crate::return_abi::XlScalarOutput::encode)
    }

    fn invoke<S: crate::RtdSubscription, C: crate::RtdSourceCatalog<S>>(
        context: &mut ReturnContext<'_, '_, Self::InputMode, S, C>,
        operation: impl FnOnce() -> XllResult<Self>,
    ) -> XllResult<Self::Payload> {
        context
            .publish_existing_alias(operation)
            .and_then(crate::return_abi::XlScalarOutput::encode)
    }
}

impl<T: ExcelHandleObject> super::MainThreadReturn for crate::handle::HandleAlias<'_, T> {}
