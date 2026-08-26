use super::excel_handle::{AsyncCompletionTracker, OwnedAsyncHandle};
use crate::cancellation::{CancellationGuarantee, CancellationSource, CancellationToken};
use crate::error::InputError;
use crate::execution::{CallId, CallMetadata, CallOutcome, UdfResultKind};
use crate::return_value::{AsyncReturnPointer, ExcelCallbackStatus, ReturnContext};
use crate::runtime::Runtime;
use crate::value::ExcelReturn;
use crate::{XllError, XllResult};
use futures_util::{Future, FutureExt};
use parking_lot::Mutex;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::Arc;
use xlfn_sys::{XLOPER12, XLTYPE_BOOL};

/// Runs the synchronous launch portion of a native Excel async UDF.
///
/// # Safety
///
/// `raw_handle` must point to a valid, aligned, Excel-owned `XLOPER12` async
/// handle that remains live for the duration of this call.
#[doc(hidden)]
pub(crate) unsafe fn async_udf_boundary_named<A, Start, Fut, T>(
    runtime: &'static Runtime<A>,
    udf_id: &'static str,
    excel_name: &'static str,
    raw_handle: *mut XLOPER12,
    start: Start,
) where
    A: crate::Addin,
    Start: FnOnce(
        &crate::runtime::CallGuard<'_, A>,
        crate::generation::ExecutionLease<A>,
        CancellationToken,
    ) -> XllResult<Fut>,
    Fut: Future<Output = XllResult<T>> + Send + 'static,
    T: ExcelReturn + Send + 'static,
{
    let ingress = match crate::module_runtime::ingress()
        .enter_udf_with(|| {})
        .into_admitted()
    {
        Ok(ingress) => ingress,
        Err(_) => return,
    };
    #[cfg(any(test, feature = "refinement"))]
    runtime
        .refinement_hooks()
        .external_entered(runtime, ingress.activity_id());

    let call = match runtime.enter(&ingress) {
        Ok(call) => call,
        Err(_) => {
            #[cfg(any(test, feature = "refinement"))]
            runtime
                .refinement_hooks()
                .external_left(runtime, ingress.activity_id());
            return;
        }
    };

    let result = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: forwarded from this function's raw-handle contract.
        unsafe {
            async_udf_boundary_named_inner(runtime, &call, udf_id, excel_name, raw_handle, start);
        }
    }));

    if result.is_err() {
        crate::diagnostics::report_no_unwind(udf_id, &XllError::Panic);
    }

    drop(call);

    #[cfg(any(test, feature = "refinement"))]
    runtime
        .refinement_hooks()
        .external_left(runtime, ingress.activity_id());
}

pub(crate) unsafe fn async_udf_boundary_named_inner<A, Start, Fut, T>(
    runtime: &'static Runtime<A>,
    guard: &crate::runtime::CallGuard<'_, A>,
    udf_id: &'static str,
    excel_name: &'static str,
    raw_handle: *mut XLOPER12,
    start: Start,
) where
    A: crate::Addin,
    Start: FnOnce(
        &crate::runtime::CallGuard<'_, A>,
        crate::generation::ExecutionLease<A>,
        CancellationToken,
    ) -> XllResult<Fut>,
    Fut: Future<Output = XllResult<T>> + Send + 'static,
    T: ExcelReturn + Send + 'static,
{
    use crate::execution::UdfLayers;

    let call_id = runtime.next_call_id();
    let timer = crate::execution::CallTimer::start();
    let started_at = std::time::SystemTime::now();

    let concurrent_calls = crate::module_runtime::ingress().active_udfs();
    let metadata = CallMetadata {
        udf_id,
        excel_name,
        call_id: CallId::new(call_id),
        calculation_id: runtime.calculation_id(),
        started_at,
        concurrent_calls,
    };
    let layers = if <A::Layers as UdfLayers>::HAS_LAYERS {
        match guard.layers().enter(&metadata) {
            Ok(layers) => Some(layers),
            Err(error) => {
                crate::diagnostics::report_no_unwind(udf_id, &error);
                // SAFETY: forwarded from this function's raw-handle contract.
                unsafe { return_error(udf_id, raw_handle, &error) };
                return;
            }
        }
    } else {
        None
    };
    let tracker = Arc::new(Mutex::new(AsyncCompletionTracker::new(
        &metadata, timer, layers,
    )));

    // Excel does not raise CalculationEnded/CalculationCanceled for every
    // programmatic recalculation, so the public token cannot promise complete
    // calculation scoping even though event-driven generations are linearized.
    let (cancellation, token) = CancellationSource::new(CancellationGuarantee::BestEffort);
    // SAFETY: forwarded from this function's raw-handle contract.
    let mut handle = match unsafe { OwnedAsyncHandle::from_raw(udf_id, raw_handle) } {
        Ok(handle) => handle,
        Err(error) => {
            tracker.lock().finish_error(&error);
            // SAFETY: forwarded from this function's raw-handle contract.
            unsafe { return_error(udf_id, raw_handle, &error) };
            return;
        }
    };
    let future = catch_unwind(AssertUnwindSafe(|| {
        start(guard, guard.lease(), token.clone())
    }))
    .unwrap_or(Err(XllError::Panic));
    match future {
        Ok(future) => {
            let tracker_task = Arc::clone(&tracker);
            let task = async move {
                let evaluated = AssertUnwindSafe(future).catch_unwind().await;
                #[cfg(test)]
                if let Some(hook) = *AFTER_ASYNC_EVALUATION_HOOK.lock() {
                    hook();
                }

                // Linearize delivery vs cancellation using CAS on the delivery state machine.
                if !token.try_start_delivery() {
                    let cancel_error = XllError::ExcelValue(crate::ExcelError::NotAvailable);
                    handle.set_error(cancel_error.clone());
                    tracker_task.lock().finish_error(&cancel_error);
                    return;
                }

                let result = match evaluated {
                    Ok(Ok(value)) => catch_unwind(AssertUnwindSafe(|| {
                        let mut return_context = ReturnContext::new();
                        let value = T::invoke(&mut return_context, || Ok(value))?;
                        AsyncReturnPointer::from_value(value)
                    }))
                    .unwrap_or(Err(XllError::Panic)),
                    Ok(Err(error)) => Err(error),
                    Err(_) => Err(XllError::Panic),
                };
                let (pointer, computation_error) = match result {
                    Ok(pointer) => (pointer, None),
                    Err(error) => (AsyncReturnPointer::error(&error), Some(error)),
                };
                // SAFETY: both pointers are owned and live for the callback.
                let delivery = unsafe {
                    let delivery = async_return(handle.pointer(), pointer.as_non_null());
                    handle.complete();
                    delivery
                };
                token.finish_delivery();
                match delivery {
                    Ok(()) => match computation_error {
                        Some(error) => tracker_task.lock().finish_error(&error),
                        None => {
                            let outcome = CallOutcome {
                                result: UdfResultKind::Success,
                                error: None,
                                vendor_code: None,
                                duration: timer.elapsed(),
                            };
                            tracker_task.lock().finish(&outcome);
                        }
                    },
                    Err(error) => tracker_task.lock().finish_error(&error),
                }
            };
            if let Err(error) =
                runtime
                    .async_manager()
                    .spawn(metadata.calculation_id.get(), task, cancellation)
            {
                tracker.lock().finish_error(&error);
            }
        }
        Err(error) => {
            handle.set_error(error.clone());
            tracker.lock().finish_error(&error);
        }
    }
}

pub(crate) unsafe fn return_error(udf_id: &'static str, handle: *mut XLOPER12, error: &XllError) {
    let Some(handle) = NonNull::new(handle) else {
        crate::diagnostics::report_no_unwind(
            udf_id,
            &XllError::input("async_handle", InputError::Malformed("null async handle")),
        );
        return;
    };
    let pointer = AsyncReturnPointer::error(error);
    // SAFETY: the RAII-owned return is live for the callback.
    unsafe {
        if let Err(delivery_error) = async_return(handle, pointer.as_non_null()) {
            crate::diagnostics::report_no_unwind(udf_id, &delivery_error);
        }
    }
}

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
#[cfg(test)]
pub(crate) static AFTER_ASYNC_EVALUATION_HOOK: Mutex<Option<fn()>> = Mutex::new(None);

pub(crate) fn cancel_async_calculation<A: crate::Addin>(runtime: &Runtime<A>) {
    runtime.cancel_async();
}

pub(crate) fn end_async_calculation<A: crate::Addin>(runtime: &Runtime<A>) {
    runtime.finish_calculation();
}
