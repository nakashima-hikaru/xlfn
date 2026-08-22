use super::boundary::return_error;
use super::manager::MAX_ASYNC_HANDLE_BYTES;
use crate::error::InputError;
use crate::execution::{CallId, CallMetadata, CallOutcome};
use crate::{XllError, XllResult};
use std::ptr::NonNull;
use xlfn_sys::{XLOPER12, XLOPER12BigData, XLOPER12BigDataHandle, XLOPER12Value, XLTYPE_BIG_DATA};

pub(crate) struct OwnedAsyncHandle {
    pub(crate) udf_id: &'static str,
    pub(crate) raw: XLOPER12,
    pub(crate) bytes: Option<Box<[u8]>>,
    pub(crate) completed: bool,
    pub(crate) fallback_error: Option<XllError>,
}

// SAFETY: construction owns any pointed-to bytes; an opaque zero-length handle
// is only copied back to Excel and is never dereferenced by Rust.
unsafe impl Send for OwnedAsyncHandle {}

impl OwnedAsyncHandle {
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
            completed: false,
            fallback_error: None,
        })
    }

    pub(crate) fn pointer(&mut self) -> NonNull<XLOPER12> {
        let _ = &self.bytes;
        NonNull::from_mut(&mut self.raw)
    }

    pub(crate) fn set_error(&mut self, error: XllError) {
        self.fallback_error = Some(error);
    }

    pub(crate) fn complete(&mut self) {
        self.completed = true;
    }
}

impl Drop for OwnedAsyncHandle {
    fn drop(&mut self) {
        if !self.completed {
            self.completed = true;
            let error = self
                .fallback_error
                .take()
                .unwrap_or(XllError::ExcelValue(crate::ExcelError::NotAvailable));
            // SAFETY: raw is live and owned by this handle.
            unsafe {
                return_error(self.udf_id, &mut self.raw, &error);
            }
        }
    }
}

pub(crate) struct AsyncCompletionTracker<G: crate::execution::UdfLayerGuard> {
    pub(crate) udf_id: &'static str,
    pub(crate) excel_name: &'static str,
    pub(crate) call_id: CallId,
    pub(crate) calculation_id: crate::execution::CalculationId,
    pub(crate) concurrent_calls: usize,
    pub(crate) timer: crate::execution::CallTimer,
    pub(crate) layers: Option<G>,
    pub(crate) completed: bool,
}

impl<G: crate::execution::UdfLayerGuard> AsyncCompletionTracker<G> {
    pub(crate) fn new(
        metadata: &CallMetadata,
        timer: crate::execution::CallTimer,
        layers: Option<G>,
    ) -> Self {
        Self {
            udf_id: metadata.udf_id,
            excel_name: metadata.excel_name,
            call_id: metadata.call_id,
            calculation_id: metadata.calculation_id,
            concurrent_calls: metadata.concurrent_calls,
            timer,
            layers,
            completed: false,
        }
    }

    pub(crate) fn finish(&mut self, outcome: &CallOutcome<'_>) {
        if !self.completed {
            self.completed = true;
            if let Some(layers) = self.layers.take() {
                layers.exit(outcome);
            }
            let trace_metadata = crate::execution::UdfTraceMetadata {
                udf_id: self.udf_id,
                excel_name: self.excel_name,
                call_id: self.call_id,
                calculation_id: self.calculation_id,
                concurrent_calls: self.concurrent_calls,
            };
            crate::execution::trace(&trace_metadata, outcome);
        }
    }

    pub(crate) fn finish_error(&mut self, error: &XllError) {
        if !self.completed {
            crate::diagnostics::report_no_unwind(self.udf_id, error);
            let outcome = crate::execution::outcome_for_error(error, self.timer.elapsed());
            self.finish(&outcome);
        }
    }
}

impl<G: crate::execution::UdfLayerGuard> Drop for AsyncCompletionTracker<G> {
    fn drop(&mut self) {
        if !self.completed {
            let error = XllError::ExcelValue(crate::ExcelError::NotAvailable);
            self.finish_error(&error);
        }
    }
}
