// module: macro_abi
use crate::__private::v1::*;
fn no_decode_after_return_context(mut frame: CallFrame<'_, crate::value::PlainInputMode>) {
    let _ = frame.return_context("udf");
    let _ = frame.native_f64(0, "number", 1.0); // error: E0382
}
