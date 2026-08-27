use super::completion::{OwnedDeliveryOutcome, return_error};
use super::manager::MAX_ASYNC_HANDLE_BYTES;
use crate::error::InputError;
use crate::return_value::AsyncReturnPointer;
use crate::{XllError, XllResult};
use std::ptr::NonNull;
use xlfn_sys::{XLOPER12, XLOPER12BigData, XLOPER12BigDataHandle, XLOPER12Value, XLTYPE_BIG_DATA};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DeliveryState {
    Pending,
    Attempted,
}

/// Owns the copied Excel async handle until its single delivery attempt.
///
/// The state is changed to `Attempted` before calling Excel. This makes the
/// fallback in `Drop` a completion path for a still-pending handle, never a
/// retry after an FFI call has started.
pub(crate) struct ExcelAsyncResponder {
    pub(crate) udf_id: &'static str,
    pub(crate) raw: XLOPER12,
    pub(crate) bytes: Option<Box<[u8]>>,
    pub(crate) fallback_error: Option<XllError>,
    state: DeliveryState,
}

// SAFETY: construction owns any pointed-to bytes; an opaque zero-length handle
// is only copied back to Excel and is never dereferenced by Rust.
unsafe impl Send for ExcelAsyncResponder {}

impl ExcelAsyncResponder {
    pub(crate) unsafe fn from_raw(udf_id: &'static str, raw: *mut XLOPER12) -> XllResult<Self> {
        // SAFETY: the caller guarantees a live Excel async-handle argument.
        let value = unsafe { raw.as_ref() }.ok_or_else(|| {
            XllError::input("async_handle", InputError::Malformed("null async handle"))
        })?;
        if value.base_type() != XLTYPE_BIG_DATA {
            return Err(XllError::input(
                "async_handle",
                InputError::Malformed("expected xltypeBigData"),
            ));
        }
        // SAFETY: XLTYPE_BIG_DATA selects the big_data union field.
        let big_data = unsafe { value.value.big_data };
        let byte_count = usize::try_from(big_data.byte_count).map_err(|_| {
            XllError::input(
                "async_handle",
                InputError::Malformed("negative async handle size"),
            )
        })?;
        if byte_count > MAX_ASYNC_HANDLE_BYTES {
            return Err(XllError::input(
                "async_handle",
                InputError::Malformed("async handle is too large"),
            ));
        }
        let mut bytes = if byte_count == 0 {
            None
        } else {
            // SAFETY: a positive byte count selects the data pointer representation.
            let data = unsafe { big_data.handle.data };
            if data.is_null() {
                return Err(XllError::input(
                    "async_handle",
                    InputError::Malformed("null async handle data"),
                ));
            }
            // SAFETY: Excel promises byte_count readable bytes for this call.
            Some(
                unsafe { std::slice::from_raw_parts(data, byte_count) }
                    .to_vec()
                    .into_boxed_slice(),
            )
        };
        let handle = bytes
            .as_mut()
            .map_or(big_data.handle, |bytes| XLOPER12BigDataHandle {
                data: bytes.as_mut_ptr(),
            });
        Ok(Self {
            udf_id,
            raw: XLOPER12 {
                value: XLOPER12Value {
                    big_data: XLOPER12BigData {
                        handle,
                        byte_count: big_data.byte_count,
                    },
                },
                xltype: XLTYPE_BIG_DATA,
            },
            bytes,
            fallback_error: None,
            state: DeliveryState::Pending,
        })
    }

    fn pointer(&mut self) -> NonNull<XLOPER12> {
        let _ = &self.bytes;
        NonNull::from_mut(&mut self.raw)
    }

    pub(crate) fn set_fallback_error(&mut self, error: XllError) {
        self.fallback_error = Some(error);
    }

    pub(crate) unsafe fn deliver(&mut self, value: AsyncReturnPointer) -> XllResult<()> {
        if self.state != DeliveryState::Pending {
            return Err(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::ASYNC_DELIVERY,
            });
        }
        // Mark the attempt before crossing the FFI boundary. A panic or error
        // from Excel must not cause Drop to issue a second callback.
        self.state = DeliveryState::Attempted;
        // SAFETY: the responder owns a valid copied handle and `value` remains
        // owned by the caller for the duration of this call.
        unsafe { super::completion::async_return(self.pointer(), value.as_non_null()) }
    }

    /// Delivers an Excel error through the same single-attempt path as a
    /// completed async result.
    pub(crate) unsafe fn deliver_error(&mut self, error: &XllError) -> XllResult<()> {
        // SAFETY: the returned pointer is owned by this call until `deliver`
        // completes, which forwards the pointer to Excel synchronously.
        unsafe { self.deliver(AsyncReturnPointer::error(error)) }
    }
}

impl Drop for ExcelAsyncResponder {
    fn drop(&mut self) {
        if self.state == DeliveryState::Pending {
            self.state = DeliveryState::Attempted;
            let error = self
                .fallback_error
                .take()
                .unwrap_or(XllError::ExcelValue(crate::ExcelError::NotAvailable));
            // SAFETY: raw is live and owned by this handle.
            let delivery = unsafe { return_error(&mut self.raw, &error) };
            if let OwnedDeliveryOutcome::Failed(delivery_error) = delivery {
                crate::diagnostics::report_no_unwind(self.udf_id, &delivery_error);
            }
        }
    }
}
