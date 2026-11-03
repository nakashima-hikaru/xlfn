// module: value
use crate::call_return::{ExcelReturn, ReturnContext};
use crate::return_abi::{XlArrayOutput, XlScalarOutput};

fn scalar(context: &mut ReturnContext<'_, '_>) -> crate::XllResult<XlScalarOutput> {
    <Result<f64, crate::XllError> as ExcelReturn>::into_excel(Ok(1.0), context)
}
fn array(context: &mut ReturnContext<'_, '_>, value: crate::value::Matrix<f64>) -> crate::XllResult<XlArrayOutput> {
    value.into_excel(context)
}
struct Object;
impl crate::handle::ExcelHandleObject for Object {}
fn publish<E: crate::RtdSubscription, C: crate::RtdSourceCatalog<E>>(
    context: &mut ReturnContext<'_, '_, crate::value::FormulaInputMode, E, C>,
) -> crate::XllResult<String> {
    context.publish_new_handle(|| Ok(Object))
}
