use super::completion::{
    AsyncCompletion, OwnedCompletionOutcome, OwnedDeliveryOutcome, execute_async_udf, return_error,
};
use super::excel_handle::ExcelAsyncResponder;
#[cfg(feature = "handles")]
use super::executor::{HandleScopedBuilder, HandleScopedTaskBuilder, ScopedTaskFuture};
use super::instrumentation::AsyncObservation;
use crate::call_return::ExcelReturn;
use crate::cancellation::{CancellationGuarantee, CancellationSource, CancellationToken};
use crate::execution::{CallId, CallMetadata, InstrumentationPlan};
use crate::panic_boundary::catch_no_unwind;
use crate::runtime::Runtime;
use crate::{XllError, XllResult};
use futures_util::Future;
#[cfg(test)]
use parking_lot::Mutex;
use std::panic::AssertUnwindSafe;
use xlfn_sys::XLOPER12;

#[cfg(feature = "handles")]
struct InstrumentedHandleTask<Build, G: crate::execution::UdfLayerGuard, T> {
    build: Build,
    responder: ExcelAsyncResponder,
    token: CancellationToken,
    observation: AsyncObservation<G>,
    udf_id: &'static str,
    _result: std::marker::PhantomData<fn() -> T>,
}

#[cfg(feature = "handles")]
impl<Build, G, T> HandleScopedTaskBuilder for InstrumentedHandleTask<Build, G, T>
where
    Build: HandleScopedBuilder<T>,
    G: crate::execution::UdfLayerGuard,
    T: ExcelReturn + Send + 'static,
{
    fn build_task<'generation>(
        self,
        scope: super::executor::AsyncTaskScope<'generation>,
    ) -> ScopedTaskFuture<'generation> {
        let future = self.build.build(scope);
        Box::pin(async move {
            let completion = execute_async_udf(self.responder, self.token, future).await;
            report_completion(self.udf_id, &completion);
            self.observation.finish(&completion);
        })
    }
}

#[cfg(feature = "handles")]
struct UninstrumentedHandleTask<Build, T> {
    build: Build,
    responder: ExcelAsyncResponder,
    token: CancellationToken,
    udf_id: &'static str,
    _result: std::marker::PhantomData<fn() -> T>,
}

#[cfg(feature = "handles")]
impl<Build, T> HandleScopedTaskBuilder for UninstrumentedHandleTask<Build, T>
where
    Build: HandleScopedBuilder<T>,
    T: ExcelReturn + Send + 'static,
{
    fn build_task<'generation>(
        self,
        scope: super::executor::AsyncTaskScope<'generation>,
    ) -> ScopedTaskFuture<'generation> {
        let future = self.build.build(scope);
        Box::pin(async move {
            let completion = execute_async_udf(self.responder, self.token, future).await;
            report_completion(self.udf_id, &completion);
        })
    }
}

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
    let _external = runtime.observer().observe_external();

    let call = match runtime.enter(&ingress) {
        Ok(call) => call,
        Err(_) => return,
    };

    let result = catch_no_unwind(AssertUnwindSafe(|| {
        // SAFETY: forwarded from this function's raw-handle contract.
        unsafe {
            async_udf_boundary_named_inner(runtime, &call, udf_id, excel_name, raw_handle, start);
        }
    }));

    if result.is_err() {
        crate::diagnostics::report_no_unwind(udf_id, &XllError::Panic);
    }

    drop(call);
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
    let instrumentation = InstrumentationPlan::for_call(guard);
    if instrumentation.enabled() {
        // SAFETY: forwarded from this function's raw-handle contract.
        unsafe {
            async_udf_boundary_instrumented(
                runtime,
                guard,
                udf_id,
                excel_name,
                raw_handle,
                start,
                instrumentation,
            );
        }
    } else {
        // SAFETY: forwarded from this function's raw-handle contract.
        unsafe {
            async_udf_boundary_uninstrumented(runtime, guard, udf_id, raw_handle, start);
        }
    }
}

unsafe fn async_udf_boundary_instrumented<A, Start, Fut, T>(
    runtime: &'static Runtime<A>,
    guard: &crate::runtime::CallGuard<'_, A>,
    udf_id: &'static str,
    excel_name: &'static str,
    raw_handle: *mut XLOPER12,
    start: Start,
    instrumentation: InstrumentationPlan<A>,
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
    let call_id = CallId::new(runtime.next_call_id());
    let timer = crate::execution::CallTimer::start();
    let calculation_id = runtime.calculation_id();
    let concurrent_calls = crate::module_runtime::ingress().active_udfs();
    let metadata = CallMetadata {
        udf_id,
        excel_name,
        call_id,
        calculation_id,
        started_at: std::time::SystemTime::now(),
        concurrent_calls,
    };
    let layers = if instrumentation.has_layers() {
        match crate::execution::enter_layers(guard.layers(), &metadata) {
            Ok(layers) => Some(layers),
            Err(error) => {
                // SAFETY: forwarded from this function's raw-handle contract.
                let delivery = unsafe { return_error(raw_handle, &error) };
                let completion = AsyncCompletion {
                    completion: OwnedCompletionOutcome::Error(error),
                    delivery,
                };
                report_completion(udf_id, &completion);
                return;
            }
        }
    } else {
        None
    };
    // Excel does not raise CalculationEnded/CalculationCanceled for every
    // programmatic recalculation, so the public token cannot promise complete
    // calculation scoping even though event-driven generations are linearized.
    let (cancellation, token) = CancellationSource::new(CancellationGuarantee::BestEffort);
    let observation = AsyncObservation::new(
        &metadata,
        timer,
        layers,
        instrumentation.trace_enabled(),
        token,
    );
    // SAFETY: forwarded from this function's raw-handle contract.
    let mut responder = match unsafe { ExcelAsyncResponder::from_raw(udf_id, raw_handle) } {
        Ok(responder) => responder,
        Err(error) => {
            // SAFETY: forwarded from this function's raw-handle contract.
            let delivery = unsafe { return_error(raw_handle, &error) };
            let completion = AsyncCompletion {
                completion: OwnedCompletionOutcome::Error(error),
                delivery,
            };
            report_completion(udf_id, &completion);
            observation.finish(&completion);
            return;
        }
    };
    let future = catch_no_unwind(AssertUnwindSafe(|| {
        start(guard, runtime.execution_lease(guard), token)
    }))
    .unwrap_or(Err(XllError::Panic));
    match future {
        Ok(future) => {
            let reservation = match runtime.async_manager().reserve_spawn(calculation_id.get()) {
                Ok(reservation) => reservation,
                Err(error) => {
                    responder.set_fallback_error(error.clone());
                    drop(responder);
                    let completion = AsyncCompletion {
                        completion: OwnedCompletionOutcome::Error(error),
                        delivery: OwnedDeliveryOutcome::Unobserved,
                    };
                    report_completion(udf_id, &completion);
                    observation.finish(&completion);
                    return;
                }
            };
            let task = async move {
                let completion = execute_async_udf(responder, token, future).await;
                report_completion(udf_id, &completion);
                observation.finish(&completion);
            };
            reservation.commit(task, cancellation);
        }
        Err(error) => {
            responder.set_fallback_error(error.clone());
            drop(responder);
            let completion = AsyncCompletion {
                completion: OwnedCompletionOutcome::Error(error),
                delivery: OwnedDeliveryOutcome::Unobserved,
            };
            report_completion(udf_id, &completion);
            observation.finish(&completion);
        }
    }
}

unsafe fn async_udf_boundary_uninstrumented<A, Start, Fut, T>(
    runtime: &'static Runtime<A>,
    guard: &crate::runtime::CallGuard<'_, A>,
    udf_id: &'static str,
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
    let calculation_id = runtime.calculation_id();
    let (cancellation, token) = CancellationSource::new(CancellationGuarantee::BestEffort);
    // SAFETY: forwarded from this function's raw-handle contract.
    let mut responder = match unsafe { ExcelAsyncResponder::from_raw(udf_id, raw_handle) } {
        Ok(responder) => responder,
        Err(error) => {
            // SAFETY: forwarded from this function's raw-handle contract.
            let delivery = unsafe { return_error(raw_handle, &error) };
            let completion = AsyncCompletion {
                completion: OwnedCompletionOutcome::Error(error),
                delivery,
            };
            report_completion(udf_id, &completion);
            return;
        }
    };
    let future = catch_no_unwind(AssertUnwindSafe(|| {
        start(guard, runtime.execution_lease(guard), token)
    }))
    .unwrap_or(Err(XllError::Panic));
    match future {
        Ok(future) => {
            let reservation = match runtime.async_manager().reserve_spawn(calculation_id.get()) {
                Ok(reservation) => reservation,
                Err(error) => {
                    responder.set_fallback_error(error.clone());
                    drop(responder);
                    let completion = AsyncCompletion {
                        completion: OwnedCompletionOutcome::Error(error),
                        delivery: OwnedDeliveryOutcome::Unobserved,
                    };
                    report_completion(udf_id, &completion);
                    return;
                }
            };
            let task = async move {
                let completion = execute_async_udf(responder, token, future).await;
                report_completion(udf_id, &completion);
            };
            reservation.commit(task, cancellation);
        }
        Err(error) => {
            responder.set_fallback_error(error.clone());
            drop(responder);
            let completion = AsyncCompletion {
                completion: OwnedCompletionOutcome::Error(error),
                delivery: OwnedDeliveryOutcome::Unobserved,
            };
            report_completion(udf_id, &completion);
        }
    }
}

/// Specialized async-UDF boundary for generation-scoped `HandleLease`
/// parameters. The start closure decodes owned pending pins during the Excel
/// call; the builder brands those pins only after spawn admission succeeds.
#[doc(hidden)]
#[cfg(feature = "handles")]
pub(crate) unsafe fn async_udf_boundary_named_handle<A, Start, Build, T>(
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
    ) -> XllResult<Build>,
    Build: HandleScopedBuilder<T> + Send + 'static,
    T: ExcelReturn + Send + 'static,
{
    let ingress = match crate::module_runtime::ingress()
        .enter_udf_with(|| {})
        .into_admitted()
    {
        Ok(ingress) => ingress,
        Err(_) => return,
    };
    let _external = runtime.observer().observe_external();

    let call = match runtime.enter(&ingress) {
        Ok(call) => call,
        Err(_) => return,
    };

    let result = catch_no_unwind(AssertUnwindSafe(|| {
        // SAFETY: forwarded from this function's raw-handle contract.
        unsafe {
            async_udf_boundary_named_handle_inner(
                runtime, &call, udf_id, excel_name, raw_handle, start,
            );
        }
    }));

    if result.is_err() {
        crate::diagnostics::report_no_unwind(udf_id, &XllError::Panic);
    }

    drop(call);
}

#[cfg(feature = "handles")]
unsafe fn async_udf_boundary_named_handle_inner<A, Start, Build, T>(
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
    ) -> XllResult<Build>,
    Build: HandleScopedBuilder<T> + Send + 'static,
    T: ExcelReturn + Send + 'static,
{
    let instrumentation = InstrumentationPlan::for_call(guard);
    if instrumentation.enabled() {
        // SAFETY: forwarded from this function's raw-handle contract.
        unsafe {
            async_udf_boundary_instrumented_handle(
                runtime,
                guard,
                udf_id,
                excel_name,
                raw_handle,
                start,
                instrumentation,
            );
        }
    } else {
        // SAFETY: forwarded from this function's raw-handle contract.
        unsafe {
            async_udf_boundary_uninstrumented_handle(runtime, guard, udf_id, raw_handle, start);
        }
    }
}

#[cfg(feature = "handles")]
unsafe fn async_udf_boundary_instrumented_handle<A, Start, Build, T>(
    runtime: &'static Runtime<A>,
    guard: &crate::runtime::CallGuard<'_, A>,
    udf_id: &'static str,
    excel_name: &'static str,
    raw_handle: *mut XLOPER12,
    start: Start,
    instrumentation: InstrumentationPlan<A>,
) where
    A: crate::Addin,
    Start: FnOnce(
        &crate::runtime::CallGuard<'_, A>,
        crate::generation::ExecutionLease<A>,
        CancellationToken,
    ) -> XllResult<Build>,
    Build: HandleScopedBuilder<T> + Send + 'static,
    T: ExcelReturn + Send + 'static,
{
    let call_id = CallId::new(runtime.next_call_id());
    let timer = crate::execution::CallTimer::start();
    let calculation_id = runtime.calculation_id();
    let concurrent_calls = crate::module_runtime::ingress().active_udfs();
    let metadata = CallMetadata {
        udf_id,
        excel_name,
        call_id,
        calculation_id,
        started_at: std::time::SystemTime::now(),
        concurrent_calls,
    };
    let layers = if instrumentation.has_layers() {
        match crate::execution::enter_layers(guard.layers(), &metadata) {
            Ok(layers) => Some(layers),
            Err(error) => {
                // SAFETY: forwarded from this function's raw-handle contract.
                let delivery = unsafe { return_error(raw_handle, &error) };
                let completion = AsyncCompletion {
                    completion: OwnedCompletionOutcome::Error(error),
                    delivery,
                };
                report_completion(udf_id, &completion);
                return;
            }
        }
    } else {
        None
    };
    // The public cancellation token remains best effort; the task drain is
    // the stronger lifetime guarantee for raw object pins.
    let (cancellation, token) = CancellationSource::new(CancellationGuarantee::BestEffort);
    let observation = AsyncObservation::new(
        &metadata,
        timer,
        layers,
        instrumentation.trace_enabled(),
        token,
    );
    // SAFETY: forwarded from this function's raw-handle contract.
    let mut responder = match unsafe { ExcelAsyncResponder::from_raw(udf_id, raw_handle) } {
        Ok(responder) => responder,
        Err(error) => {
            // SAFETY: forwarded from this function's raw-handle contract.
            let delivery = unsafe { return_error(raw_handle, &error) };
            let completion = AsyncCompletion {
                completion: OwnedCompletionOutcome::Error(error),
                delivery,
            };
            report_completion(udf_id, &completion);
            observation.finish(&completion);
            return;
        }
    };
    let prepared = catch_no_unwind(AssertUnwindSafe(|| {
        let lease = runtime.execution_lease(guard);
        let generation = lease.generation();
        start(guard, lease, token).map(|build| (generation, build))
    }))
    .unwrap_or(Err(XllError::Panic));
    match prepared {
        Ok((generation, build)) => {
            let reservation = match runtime.async_manager().reserve_spawn(calculation_id.get()) {
                Ok(reservation) => reservation,
                Err(error) => {
                    responder.set_fallback_error(error.clone());
                    drop(responder);
                    let completion = AsyncCompletion {
                        completion: OwnedCompletionOutcome::Error(error),
                        delivery: OwnedDeliveryOutcome::Unobserved,
                    };
                    report_completion(udf_id, &completion);
                    observation.finish(&completion);
                    return;
                }
            };
            reservation.commit_handle_scoped(
                generation,
                InstrumentedHandleTask {
                    build,
                    responder,
                    token,
                    observation,
                    udf_id,
                    _result: std::marker::PhantomData,
                },
                cancellation,
            );
        }
        Err(error) => {
            responder.set_fallback_error(error.clone());
            drop(responder);
            let completion = AsyncCompletion {
                completion: OwnedCompletionOutcome::Error(error),
                delivery: OwnedDeliveryOutcome::Unobserved,
            };
            report_completion(udf_id, &completion);
            observation.finish(&completion);
        }
    }
}

#[cfg(feature = "handles")]
unsafe fn async_udf_boundary_uninstrumented_handle<A, Start, Build, T>(
    runtime: &'static Runtime<A>,
    guard: &crate::runtime::CallGuard<'_, A>,
    udf_id: &'static str,
    raw_handle: *mut XLOPER12,
    start: Start,
) where
    A: crate::Addin,
    Start: FnOnce(
        &crate::runtime::CallGuard<'_, A>,
        crate::generation::ExecutionLease<A>,
        CancellationToken,
    ) -> XllResult<Build>,
    Build: HandleScopedBuilder<T> + Send + 'static,
    T: ExcelReturn + Send + 'static,
{
    let calculation_id = runtime.calculation_id();
    let (cancellation, token) = CancellationSource::new(CancellationGuarantee::BestEffort);
    // SAFETY: forwarded from this function's raw-handle contract.
    let mut responder = match unsafe { ExcelAsyncResponder::from_raw(udf_id, raw_handle) } {
        Ok(responder) => responder,
        Err(error) => {
            // SAFETY: forwarded from this function's raw-handle contract.
            let delivery = unsafe { return_error(raw_handle, &error) };
            let completion = AsyncCompletion {
                completion: OwnedCompletionOutcome::Error(error),
                delivery,
            };
            report_completion(udf_id, &completion);
            return;
        }
    };
    let prepared = catch_no_unwind(AssertUnwindSafe(|| {
        let lease = runtime.execution_lease(guard);
        let generation = lease.generation();
        start(guard, lease, token).map(|build| (generation, build))
    }))
    .unwrap_or(Err(XllError::Panic));
    match prepared {
        Ok((generation, build)) => {
            let reservation = match runtime.async_manager().reserve_spawn(calculation_id.get()) {
                Ok(reservation) => reservation,
                Err(error) => {
                    responder.set_fallback_error(error.clone());
                    drop(responder);
                    let completion = AsyncCompletion {
                        completion: OwnedCompletionOutcome::Error(error),
                        delivery: OwnedDeliveryOutcome::Unobserved,
                    };
                    report_completion(udf_id, &completion);
                    return;
                }
            };
            reservation.commit_handle_scoped(
                generation,
                UninstrumentedHandleTask {
                    build,
                    responder,
                    token,
                    udf_id,
                    _result: std::marker::PhantomData,
                },
                cancellation,
            );
        }
        Err(error) => {
            responder.set_fallback_error(error.clone());
            drop(responder);
            let completion = AsyncCompletion {
                completion: OwnedCompletionOutcome::Error(error),
                delivery: OwnedDeliveryOutcome::Unobserved,
            };
            report_completion(udf_id, &completion);
        }
    }
}

fn report_completion(udf_id: &'static str, completion: &AsyncCompletion) {
    if let OwnedCompletionOutcome::Error(error) = &completion.completion {
        crate::diagnostics::report_no_unwind(udf_id, error);
    }
    if let OwnedDeliveryOutcome::Failed(error) = &completion.delivery {
        crate::diagnostics::report_no_unwind(udf_id, error);
    }
}
#[cfg(test)]
pub(crate) static AFTER_ASYNC_EVALUATION_HOOK: Mutex<Option<fn()>> = Mutex::new(None);

pub(crate) fn cancel_async_calculation<A: crate::Addin>(runtime: &Runtime<A>) {
    runtime.cancel_async();
}

pub(crate) fn end_async_calculation<A: crate::Addin>(runtime: &Runtime<A>) {
    runtime.finish_calculation();
}
