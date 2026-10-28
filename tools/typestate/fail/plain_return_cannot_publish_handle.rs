// module: value
use crate::call_return::ReturnContext;
struct Object;
impl crate::handle::ExcelHandleObject for Object {}
fn invalid(context: &mut ReturnContext<'_, '_>) {
    let _ = context.publish_new_handle(|| Ok(Object)); // error: E0599
}
