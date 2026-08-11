use super::*;

/// Runs the synchronous launch portion of a native Excel async UDF.
///
/// # Safety
///
/// `raw_handle` must point to a valid, aligned, Excel-owned `XLOPER12` async
/// handle that remains live for the duration of this call.
#[doc(hidden)]
pub unsafe fn async_udf_boundary_named<S, Start, Fut, T>(
    runtime: &'static Runtime<S>,
    udf_id: &'static str,
    excel_name: &'static str,
    raw_handle: *mut XLOPER12,
    start: Start,
) where
    S: Send + Sync + 'static,
    Start: FnOnce(Arc<S>, CancellationToken) -> XllResult<Fut>,
    Fut: Future<Output = XllResult<T>> + Send + 'static,
    T: IntoExcelValue + Send + 'static,
{
    let (_export_guard, accepted, _concurrent_calls) = crate::ingress::global_ingress()
        .enter_udf_with(|| {
            #[cfg(any(test, feature = "shutdown-refinement"))]
            runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::EnterExternal);
        });

    if !accepted {
        return;
    }

    let call = match runtime.enter() {
        Ok(call) => call,
        Err(_) => {
            #[cfg(any(test, feature = "shutdown-refinement"))]
            runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::LeaveExternal);
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

    #[cfg(any(test, feature = "shutdown-refinement"))]
    runtime.record_ghost_event(crate::shutdown_refinement::GhostEvent::LeaveExternal);
}

pub(crate) unsafe fn async_udf_boundary_named_inner<S, Start, Fut, T>(
    runtime: &'static Runtime<S>,
    guard: &crate::runtime::CallGuard<'_, S>,
    udf_id: &'static str,
    excel_name: &'static str,
    raw_handle: *mut XLOPER12,
    start: Start,
) where
    S: Send + Sync + 'static,
    Start: FnOnce(Arc<S>, CancellationToken) -> XllResult<Fut>,
    Fut: Future<Output = XllResult<T>> + Send + 'static,
    T: IntoExcelValue + Send + 'static,
{
    let call_id = runtime.next_call_id();
    let timer = crate::execution::CallTimer::start();
    let started_at = std::time::SystemTime::now();

    let concurrent_calls = guard.concurrent_calls();
    let metadata = CallMetadata {
        udf_id,
        excel_name,
        call_id: CallId::from(call_id),
        calculation_id: runtime.calculation_id(),
        started_at,
        concurrent_calls,
    };
    let configured_layers = runtime
        .layers_if_configured()
        .unwrap_or_else(|| Arc::new(Vec::new()));
    let layers = match crate::execution::EnteredLayers::enter(&configured_layers, &metadata) {
        Ok(layers) => layers,
        Err(error) => {
            crate::diagnostics::report_no_unwind(udf_id, &error);
            // SAFETY: forwarded from this function's raw-handle contract.
            unsafe { return_error(udf_id, raw_handle, &error) };
            return;
        }
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
    let future = catch_unwind(AssertUnwindSafe(|| start(guard.state_arc(), token.clone())))
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
                        let value = value.into_excel_value()?;
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
            &XllError::input(
                "async_handle",
                crate::InputError::Malformed("null async handle"),
            ),
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
    let invocation = crate::callback_gate::CallbackInvocationToken::new();
    let callback_gate =
        crate::callback_gate::enter_callback(&invocation).map_err(|suppressed| {
            XllError::ExcelApi {
                function: "xlAsyncReturn(suppressed)",
                code: suppressed.status.raw_code(),
            }
        })?;
    // SAFETY: both XLOPER12 pointers are live for this call. The specialized
    // raw wrapper intentionally does not expose the worker-thread-forbidden
    // xlFree cleanup path.
    let (raw_status, callback_result, invoked) =
        unsafe { xlfn_sys::excel12_async_return(handle, result) };
    let status = crate::ExcelCallbackStatus::from_raw(raw_status);
    callback_gate.observe(status);
    drop(callback_gate);
    let accepted = invoked
        && status == crate::ExcelCallbackStatus::Success
        && callback_result.base_type() == XLTYPE_BOOL
        // SAFETY: XLTYPE_BOOL selects the boolean union field.
        && unsafe { callback_result.value.boolean != 0 };
    if !accepted {
        let error = XllError::ExcelApi {
            function: "xlAsyncReturn",
            code: if !invoked || status == crate::ExcelCallbackStatus::Success {
                -1
            } else {
                status.raw_code()
            },
        };
        return Err(error);
    }
    Ok(())
}
#[cfg(test)]
pub(crate) static AFTER_ASYNC_EVALUATION_HOOK: Mutex<Option<fn()>> = Mutex::new(None);

pub fn cancel_async_calculation<S>(runtime: &Runtime<S>) {
    runtime.cancel_async();
}

pub fn end_async_calculation<S>(runtime: &Runtime<S>) {
    runtime.finish_calculation();
}
