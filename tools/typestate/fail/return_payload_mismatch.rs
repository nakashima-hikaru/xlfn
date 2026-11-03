// module: value
use crate::call_return::{ExcelReturn, ReturnContext};
fn invalid(context: &mut ReturnContext<'_, '_>) {
    let _: crate::return_abi::XlArrayOutput = 1.0_f64.into_excel(context).unwrap(); // error: E0308
}
