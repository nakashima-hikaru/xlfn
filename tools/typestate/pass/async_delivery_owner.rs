// module: async_udf
use super::transport::ExcelAsyncResponder;
use crate::return_abi::AsyncReturnValue;

unsafe fn deliver(responder: ExcelAsyncResponder, value: AsyncReturnValue) {
    // SAFETY: the framework-owned responder and value retain the callback operands.
    let _ = unsafe { responder.deliver(value) };
}

unsafe fn deliver_error(responder: ExcelAsyncResponder, error: &crate::XllError) {
    // SAFETY: the framework-owned responder retains the copied callback handle.
    let _ = unsafe { responder.deliver_error(error) };
}
