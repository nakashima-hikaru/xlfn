use super::excel_handle::ExcelAsyncResponder;
use crate::cancellation::CancellationToken;
use crate::return_value::{AsyncReturnPointer, ExcelCallbackStatus, ReturnContext};
use crate::value::ExcelReturn;
use crate::{XllError, XllResult};
use futures_util::{Future, FutureExt};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use xlfn_sys::{XLOPER12, XLTYPE_BOOL};

/// The owned semantic result of one asynchronous UDF execution.
///
/// Completion and Excel delivery are independent planes. Keeping them in one
/// record prevents a delivery failure from replacing an earlier computation
/// error.
pub(crate) struct AsyncCompletion {
    pub(crate) completion: OwnedCompletionOutcome,
    pub(crate) delivery: OwnedDeliveryOutcome,
}

pub(crate) enum OwnedCompletionOutcome {
    Success,
    Error(XllError),
    Cancelled,
}

pub(crate) enum OwnedDeliveryOutcome {
    Delivered,
    Failed(XllError),
    Unobserved,
}

/// Evaluates an async UDF and attempts exactly one Excel delivery.
///
/// The responder marks its delivery state before crossing the Excel FFI
/// boundary. If cancellation wins first, the cancellation result is delivered
/// explicitly and the responder remains the last-resort fallback for forced
/// task drops.
pub(crate) async fn execute_async_udf<Fut, T>(
    mut responder: ExcelAsyncResponder,
    token: CancellationToken,
    future: Fut,
) -> AsyncCompletion
where
    Fut: Future<Output = XllResult<T>> + Send + 'static,
    T: ExcelReturn + Send + 'static,
{
    let evaluated = AssertUnwindSafe(future).catch_unwind().await;
    #[cfg(test)]
    if let Some(hook) = *super::boundary::AFTER_ASYNC_EVALUATION_HOOK.lock() {
        hook();
    }

    if !token.try_start_delivery() {
        let cancel_error = XllError::ExcelValue(crate::ExcelError::NotAvailable);
        // SAFETY: the responder owns the copied async handle and the error
        // pointer remains live for the synchronous delivery call.
        let delivery = match unsafe { responder.deliver_error(&cancel_error) } {
            Ok(()) => OwnedDeliveryOutcome::Delivered,
            Err(error) => OwnedDeliveryOutcome::Failed(error),
        };
        return AsyncCompletion {
            completion: OwnedCompletionOutcome::Cancelled,
            delivery,
        };
    }

    let (pointer, completion) = match evaluated {
        Ok(Ok(value)) => {
            let result = catch_unwind(AssertUnwindSafe(|| {
                let mut return_context = ReturnContext::new();
                let value = T::invoke(&mut return_context, || Ok(value))?;
                AsyncReturnPointer::from_value(value)
            }))
            .unwrap_or(Err(XllError::Panic));
            match result {
                Ok(pointer) => (pointer, OwnedCompletionOutcome::Success),
                Err(error) => (
                    AsyncReturnPointer::error(&error),
                    OwnedCompletionOutcome::Error(error),
                ),
            }
        }
        Ok(Err(error)) => (
            AsyncReturnPointer::error(&error),
            OwnedCompletionOutcome::Error(error),
        ),
        Err(_) => {
            let error = XllError::Panic;
            (
                AsyncReturnPointer::error(&error),
                OwnedCompletionOutcome::Error(error),
            )
        }
    };

    // SAFETY: both pointers are owned and live for the callback. The responder
    // records Attempted before invoking Excel, so a panic or error cannot cause
    // its Drop implementation to retry the delivery.
    let delivery = unsafe { responder.deliver(pointer) };
    token.finish_delivery();
    let delivery = match delivery {
        Ok(()) => OwnedDeliveryOutcome::Delivered,
        Err(error) => OwnedDeliveryOutcome::Failed(error),
    };
    AsyncCompletion {
        completion,
        delivery,
    }
}

/// Delivers an error for a raw async handle before ownership can be wrapped in
/// an [`ExcelAsyncResponder`].
pub(crate) unsafe fn return_error(handle: *mut XLOPER12, error: &XllError) -> OwnedDeliveryOutcome {
    let Some(handle) = NonNull::new(handle) else {
        return OwnedDeliveryOutcome::Unobserved;
    };
    let pointer = AsyncReturnPointer::error(error);
    // SAFETY: the raw handle is valid under the caller's FFI contract and the
    // return pointer remains owned until async_return returns.
    match unsafe { async_return(handle, pointer.as_non_null()) } {
        Ok(()) => OwnedDeliveryOutcome::Delivered,
        Err(error) => OwnedDeliveryOutcome::Failed(error),
    }
}

/// Calls Excel's async-return primitive behind callback admission.
pub(crate) unsafe fn async_return(
    handle: NonNull<XLOPER12>,
    result: NonNull<XLOPER12>,
) -> XllResult<()> {
    let callback_admission =
        crate::callback_gate::enter_callback().map_err(|suppressed| XllError::ExcelApi {
            function: crate::error::ExcelApiFunction::AsyncReturn,
            failure: crate::error::ExcelApiFailure::Suppressed(suppressed.status),
        })?;
    // SAFETY: both XLOPER12 pointers are live for this call. The specialized
    // raw wrapper intentionally does not expose the worker-thread-forbidden
    // xlFree cleanup path.
    let (raw_status, callback_result, invoked) =
        unsafe { xlfn_sys::excel12_async_return(handle, result) };
    let status = ExcelCallbackStatus::from_raw(raw_status);
    drop(callback_admission);
    let accepted = invoked
        && status == ExcelCallbackStatus::Success
        && callback_result.base_type() == XLTYPE_BOOL
        // SAFETY: XLTYPE_BOOL selects the boolean union field.
        && unsafe { callback_result.value.boolean != 0 };
    if !accepted {
        let failure = if !invoked || status == ExcelCallbackStatus::Success {
            crate::error::ExcelApiFailure::UnexpectedResult
        } else {
            crate::error::ExcelApiFailure::Status(status)
        };
        let error = XllError::ExcelApi {
            function: crate::error::ExcelApiFunction::AsyncReturn,
            failure,
        };
        return Err(error);
    }
    Ok(())
}
