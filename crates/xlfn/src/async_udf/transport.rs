use super::completion::{OwnedDeliveryOutcome, return_error};
use crate::error::InputError;
use crate::return_abi::AsyncReturnValue;
use crate::{XllError, XllResult};
use std::ptr::NonNull;
use xlfn_kernel::rollback::{Rollback, RollbackGuard};
use xlfn_sys::{XLOPER12, XLOPER12BigData, XLOPER12Value, XLTYPE_BIG_DATA};

/// Owns the copied Excel async handle until its single delivery attempt.
///
/// Delivery consumes the pending handle before entering the Excel callback path.
/// Dropping an armed responder delivers the fallback error.
#[must_use = "deliver the async response or drop it to return the fallback error"]
pub(crate) struct ExcelAsyncResponder {
    pending: RollbackGuard<PendingResponse>,
}

struct PendingResponse {
    udf_id: &'static str,
    handle: XLOPER12,
}

impl Rollback for PendingResponse {
    fn rollback(self) {
        let Self { udf_id, mut handle } = self;
        let error = XllError::ExcelValue(crate::ExcelError::NotAvailable);
        // SAFETY: the copied handle remains live and uniquely owned here.
        let delivery = unsafe { return_error(&mut handle, &error) };
        if let OwnedDeliveryOutcome::Failed(delivery_error) = delivery {
            crate::diagnostics::report_no_unwind(udf_id, &delivery_error);
        }
    }
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
            pending: RollbackGuard::new(PendingResponse {
                udf_id,
                handle: XLOPER12 {
                    value: XLOPER12Value {
                        big_data: XLOPER12BigData {
                            handle: big_data.handle,
                            byte_count: big_data.byte_count,
                        },
                    },
                    xltype: XLTYPE_BIG_DATA,
                },
            }),
        })
    }

    pub(crate) fn udf_id(&self) -> &'static str {
        self.pending.get().udf_id
    }

    #[cfg(all(test, feature = "async-builtin"))]
    pub(crate) fn pending_handle(&self) -> &XLOPER12 {
        &self.pending.get().handle
    }

    pub(crate) unsafe fn deliver(self, mut value: AsyncReturnValue) -> XllResult<()> {
        // Consume fallback authority before callback admission and the FFI call.
        let PendingResponse { mut handle, .. } = self.pending.complete();
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

#[cfg(test)]
mod tests {
    use super::*;

    fn async_handle() -> XLOPER12 {
        XLOPER12 {
            value: XLOPER12Value {
                big_data: XLOPER12BigData {
                    handle: xlfn_sys::XLOPER12BigDataHandle {
                        data: std::ptr::null_mut(),
                    },
                    byte_count: 0,
                },
            },
            xltype: XLTYPE_BIG_DATA,
        }
    }

    #[test]
    fn miri_abandoned_responder_returns_one_fallback_during_unwind() {
        let _callbacks = crate::test_callback::lock();
        crate::test_callback::install();
        crate::test_callback::reset();
        let result = crate::panic_boundary::catch_no_unwind(|| {
            let mut raw = async_handle();
            // SAFETY: raw is a live initialized handle copied by the responder.
            let _responder =
                unsafe { ExcelAsyncResponder::from_raw("DELIVERY.ABANDONED", &mut raw) }.unwrap();
            panic!("abandon pending response");
        });
        assert!(result.is_err());
        assert_eq!(crate::test_callback::async_return_calls(), 1);
    }

    #[test]
    #[cfg(feature = "async-builtin")]
    fn delivery_consumes_responder_even_when_excel_rejects() {
        let _callbacks = crate::test_callback::lock();
        crate::test_callback::install();
        for rejected in [false, true] {
            crate::test_callback::reset();
            crate::test_callback::set_async_rejected(rejected);
            let mut raw = async_handle();
            // SAFETY: raw is a live initialized handle copied by the responder.
            let responder =
                unsafe { ExcelAsyncResponder::from_raw("DELIVERY.ONCE", &mut raw) }.unwrap();
            // SAFETY: the responder owns the copied handle through the callback.
            let result = unsafe { responder.deliver_error(&XllError::Closing) };
            assert_eq!(result.is_err(), rejected);
            // Delivery consumes fallback authority even on callback failure.
            assert_eq!(crate::test_callback::async_return_calls(), 1);
        }
    }

    #[test]
    fn malformed_async_handles_never_reach_the_excel_callback() {
        let _callbacks = crate::test_callback::lock();
        crate::test_callback::install();
        crate::test_callback::reset();
        let mut unknown_flags = async_handle();
        unknown_flags.xltype |= 0x8000_0000;
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
