use xlfn::__private::v1::{CallFrame, PlainInputMode, ReturnContext};

fn main() {
    let _frame_constructor = CallFrame::<PlainInputMode>::new::<()>;
    let _return_constructor = ReturnContext::for_call::<()>;
}
