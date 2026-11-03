// module: macro_abi
use crate::__private::v1::*;
fn cannot_finish_twice(frame: CallFrame<'_, '_, crate::value::PlainInputMode>) {
    let _ = frame.finish_arguments();
    let _ = frame.finish_arguments(); // error: E0382
}
