//! Formula-handle return dispatch.

use super::{ReturnContext, ReturnPayload};
use crate::XllResult;
use crate::handle::ExcelHandleObject;
use crate::value::FormulaInputMode;

impl<T: ExcelHandleObject> super::ExcelReturnSealed for crate::handle::HandleAlias<'_, T> {}

impl<'call, T: ExcelHandleObject> super::ExcelReturn for crate::handle::HandleAlias<'call, T> {
    type InputMode = FormulaInputMode;

    fn into_excel(self, context: &mut ReturnContext<'_, '_>) -> XllResult<ReturnPayload> {
        context
            .publish_existing_alias(|| Ok(self))
            .and_then(ReturnPayload::scalar)
    }

    fn invoke(
        context: &mut ReturnContext<'_, '_>,
        operation: impl FnOnce() -> XllResult<Self>,
    ) -> XllResult<ReturnPayload> {
        context
            .publish_existing_alias(operation)
            .and_then(ReturnPayload::scalar)
    }
}

impl<T: ExcelHandleObject> super::MainThreadReturn for crate::handle::HandleAlias<'_, T> {}
