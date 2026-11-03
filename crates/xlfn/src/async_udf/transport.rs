use super::completion::{OwnedDeliveryOutcome, return_error};
use crate::error::InputError;
use crate::return_abi::AsyncReturnValue;
use crate::{XllError, XllResult};
use std::ptr::NonNull;
use xlfn_sys::{XLOPER12, XLOPER12BigData, XLOPER12Value, XLTYPE_BIG_DATA};

/// Owns the copied Excel async handle until its single delivery attempt.
///
/// Delivery consumes this authority and takes the pending handle before calling
/// Excel. `Drop` completes only an abandoned pending handle, never an attempt
/// that has already crossed the FFI boundary.
#[must_use = "deliver the async response or drop it to return the fallback error"]
pub(crate) struct ExcelAsyncResponder {
    pub(crate) udf_id: &'static str,
    pending: Option<XLOPER12>,
}

// SAFETY: the opaque Excel handle is only copied back to Excel through the
// thread-safe xlAsyncReturn callback and is never dereferenced by Rust.
unsafe impl Send for ExcelAsyncResponder {}

impl ExcelAsyncResponder {
    pub(crate) unsafe fn validate_raw<'call>(raw: *mut XLOPER12) -> XllResult<&'call XLOPER12> {
        // SAFETY: the caller guarantees live storage for aligned pointers;
        // the common parser rejects null, misalignment, and unknown type bits.
        let value = unsafe { crate::value::XlValueRef::from_raw(raw) }?;
        let value = value.raw();
        if value.base_type() != XLTYPE_BIG_DATA {
            return Err(XllError::input(
                "async_handle",
                InputError::Malformed("expected xltypeBigData"),
            ));
        }
        Ok(value)
    }

    pub(crate) unsafe fn from_raw(udf_id: &'static str, raw: *mut XLOPER12) -> XllResult<Self> {
        // SAFETY: forwarded from the caller's live-handle contract.
        let value = unsafe { Self::validate_raw(raw) }?;
        // SAFETY: XLTYPE_BIG_DATA selects the big_data union field.
        let big_data = unsafe { value.value.big_data };
        // An async handle is an opaque hdata token even when cbData is nonzero.
        // Copy the token, never read it as a byte-buffer address.
        Ok(Self {
            udf_id,
            pending: Some(XLOPER12 {
                value: XLOPER12Value {
                    big_data: XLOPER12BigData {
                        handle: big_data.handle,
                        byte_count: big_data.byte_count,
                    },
                },
                xltype: XLTYPE_BIG_DATA,
            }),
        })
    }

    #[cfg(all(test, feature = "async-builtin"))]
    pub(crate) fn pending_handle(&self) -> &XLOPER12 {
        self.pending
            .as_ref()
            .expect("responder owns pending handle")
    }

    pub(crate) unsafe fn deliver(mut self, mut value: AsyncReturnValue) -> XllResult<()> {
        // Disarm fallback before crossing the FFI boundary. A panic or error
        // from Excel cannot recreate the consumed delivery authority.
        let mut handle = self.pending.take().expect("responder owns pending handle");
        // SAFETY: the responder owns a valid copied handle and `value` remains
        // owned here for the duration of this call.
        unsafe {
            super::completion::async_return(
                NonNull::from_mut(&mut handle),
                NonNull::from_mut(value.as_raw()),
            )
        }
    }

    /// Delivers an Excel error through the same single-attempt path as a
    /// completed async result.
    pub(crate) unsafe fn deliver_error(self, error: &XllError) -> XllResult<()> {
        // SAFETY: the returned pointer is owned by this call until `deliver`
        // completes, which forwards the pointer to Excel synchronously.
        unsafe { self.deliver(AsyncReturnValue::error(error)) }
    }
}

impl Drop for ExcelAsyncResponder {
    fn drop(&mut self) {
        if let Some(mut handle) = self.pending.take() {
            let error = XllError::ExcelValue(crate::ExcelError::NotAvailable);
            // SAFETY: the taken handle remains live and uniquely owned here.
            let delivery = unsafe { return_error(&mut handle, &error) };
            if let OwnedDeliveryOutcome::Failed(delivery_error) = delivery {
                crate::diagnostics::report_no_unwind(self.udf_id, &delivery_error);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(feature = "async-builtin")]
    fn delivery_consumes_responder_even_when_excel_rejects() {
        let _callbacks = crate::test_callback::lock();
        crate::test_callback::install();
        for rejected in [false, true] {
            crate::test_callback::reset();
            crate::test_callback::set_async_rejected(rejected);
            let mut raw = XLOPER12 {
                value: XLOPER12Value {
                    big_data: XLOPER12BigData {
                        handle: xlfn_sys::XLOPER12BigDataHandle {
                            data: std::ptr::null_mut(),
                        },
                        byte_count: 0,
                    },
                },
                xltype: XLTYPE_BIG_DATA,
            };
            // SAFETY: raw is a live initialized handle copied by the responder.
            let responder =
                unsafe { ExcelAsyncResponder::from_raw("DELIVERY.ONCE", &mut raw) }.unwrap();
            // SAFETY: the responder owns the copied handle through the callback.
            let result = unsafe { responder.deliver_error(&XllError::Closing) };
            assert_eq!(result.is_err(), rejected);
            // Consuming delivery has also dropped its responder. A rejected
            // callback must not cause that Drop to issue a fallback callback.
            assert_eq!(crate::test_callback::async_return_calls(), 1);
        }
    }

    #[test]
    fn malformed_async_handles_never_reach_the_excel_callback() {
        let _callbacks = crate::test_callback::lock();
        crate::test_callback::install();
        crate::test_callback::reset();
        let mut unknown_flags = XLOPER12 {
            value: XLOPER12Value {
                big_data: XLOPER12BigData {
                    handle: xlfn_sys::XLOPER12BigDataHandle {
                        data: std::ptr::null_mut(),
                    },
                    byte_count: 0,
                },
            },
            xltype: XLTYPE_BIG_DATA | 0x8000_0000,
        };
        let misaligned = std::ptr::NonNull::<XLOPER12>::dangling()
            .as_ptr()
            .wrapping_byte_add(1);
        for raw in [std::ptr::null_mut(), misaligned, &mut unknown_flags] {
            // SAFETY: malformed pointer shapes are rejected before storage is
            // accessed; the aligned non-null operand is live and initialized.
            assert!(unsafe { ExcelAsyncResponder::from_raw("MALFORMED", raw) }.is_err());
            // SAFETY: same rejected shapes must never be forwarded to Excel.
            let delivery = unsafe { return_error(raw, &XllError::Panic) };
            assert!(matches!(delivery, OwnedDeliveryOutcome::Unobserved));
        }
        assert_eq!(crate::test_callback::async_return_calls(), 0);
    }
}
