// module: async_udf
use super::transport::ExcelAsyncResponder;
use crate::return_abi::AsyncReturnValue;

unsafe fn deliver_twice(responder: ExcelAsyncResponder, value: AsyncReturnValue) {
    let _ = unsafe { responder.deliver(value) };
    let _ = unsafe { responder.deliver_error(&crate::XllError::Closing) }; // error: E0382
}

unsafe fn deliver_error_twice(responder: ExcelAsyncResponder, value: AsyncReturnValue) {
    let _ = unsafe { responder.deliver_error(&crate::XllError::Closing) };
    let _ = unsafe { responder.deliver(value) }; // error: E0382
}
