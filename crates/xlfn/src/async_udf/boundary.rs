use super::completion::{
    AsyncCompletion, AsyncDelivery, OwnedCompletionOutcome, OwnedDeliveryOutcome, return_error,
};
use super::instrumentation::AsyncObservation;
#[cfg(feature = "handles")]
use super::task_scope::{HandleScopedBuilder, HandleScopedTaskBuilder, ScopedTaskFuture};
use super::transport::ExcelAsyncResponder;
use crate::call_return::ExcelReturn;
use crate::cancellation::{CancellationGuarantee, CancellationSource, CancellationToken};
use crate::execution::{CallId, CallMetadata, InstrumentationPlan};
use crate::panic_boundary::catch_no_unwind;
use crate::runtime::Runtime;
#[cfg(test)]
use crate::sync::Mutex;
use crate::{XllError, XllResult};
use futures_util::Future;
use std::panic::AssertUnwindSafe;
use xlfn_sys::XLOPER12;

#[cfg(feature = "handles")]
struct InstrumentedHandleTask<Build, G: crate::execution::UdfLayerGuard, T> {
    build: Option<Build>,
    responder: Option<ExcelAsyncResponder>,
    token: CancellationToken,
    observation: Option<AsyncObservation<G>>,
    udf_id: &'static str,
    _result: std::marker::PhantomData<fn() -> T>,
}

#[cfg(all(
    feature = "handles",
    feature = "bench-internals",
    feature = "async-builtin"
))]
mod scoped_bench;
#[cfg(all(
    feature = "handles",
    feature = "bench-internals",
    feature = "async-builtin"
))]
pub use scoped_bench::HandleScopedDeliveryBenchmark;
#[cfg(all(test, feature = "handles", feature = "async-builtin"))]
mod scoped_tests;

#[cfg(feature = "handles")]
impl<Build, G, T> HandleScopedTaskBuilder for InstrumentedHandleTask<Build, G, T>
where
    Build: HandleScopedBuilder<T> + 'static,
    G: crate::execution::UdfLayerGuard,
    T: ExcelReturn + Send + 'static,
{
    type Output = XllResult<T>;
    type Delivery = (
        ExcelAsyncResponder,
        CancellationToken,
        AsyncObservation<G>,
        &'static str,
    );

    fn build_task<'generation>(
        mut self,
        scope: super::task_scope::AsyncTaskScope<'generation>,
    ) -> (ScopedTaskFuture<'generation, Self::Output>, Self::Delivery) {
        let build = self.build.take().expect("task owns builder");
        let future = catch_no_unwind(AssertUnwindSafe(|| build.build(scope)))
            .unwrap_or_else(|_| Box::pin(std::future::ready(Err(XllError::Panic))));
        (
            future,
            (
                self.responder.take().unwrap(),
                self.token,
                self.observation.take().unwrap(),
                self.udf_id,
            ),
        )
    }

    fn deliver(
        delivery: Self::Delivery,
        future: ScopedTaskFuture<'static, Self::Output>,
    ) -> impl Future<Output = ()> + Send + 'static {
        let (responder, token, observation, _) = delivery;
        AsyncDelivery::new(responder, token, Some(observation)).task(future)
    }
}

#[cfg(feature = "handles")]
struct UninstrumentedHandleTask<Build, T> {
    build: Option<Build>,
    responder: Option<ExcelAsyncResponder>,
    token: CancellationToken,
    udf_id: &'static str,
    _result: std::marker::PhantomData<fn() -> T>,
}

#[cfg(feature = "handles")]
impl<Build, T> HandleScopedTaskBuilder for UninstrumentedHandleTask<Build, T>
where
    Build: HandleScopedBuilder<T> + 'static,
    T: ExcelReturn + Send + 'static,
{
    type Output = XllResult<T>;
    type Delivery = (ExcelAsyncResponder, CancellationToken, &'static str);

    fn build_task<'generation>(
        mut self,
        scope: super::task_scope::AsyncTaskScope<'generation>,
    ) -> (ScopedTaskFuture<'generation, Self::Output>, Self::Delivery) {
        let build = self.build.take().expect("task owns builder");
        let future = catch_no_unwind(AssertUnwindSafe(|| build.build(scope)))
            .unwrap_or_else(|_| Box::pin(std::future::ready(Err(XllError::Panic))));
        (
            future,
            (self.responder.take().unwrap(), self.token, self.udf_id),
        )
    }

    fn deliver(
        delivery: Self::Delivery,
        future: ScopedTaskFuture<'static, Self::Output>,
    ) -> impl Future<Output = ()> + Send + 'static {
        let (responder, token, _) = delivery;
        AsyncDelivery::<()>::new(responder, token, None).task(future)
    }
}

#[cfg(feature = "handles")]
impl<Build, G: crate::execution::UdfLayerGuard, T> Drop for InstrumentedHandleTask<Build, G, T> {
    fn drop(&mut self) {
        let panicked = catch_no_unwind(AssertUnwindSafe(|| drop(self.build.take()))).is_err();
        if let Some(responder) = self.responder.take() {
            AsyncDelivery::new(responder, self.token, self.observation.take()).fallback(panicked);
        }
    }
}

#[cfg(feature = "handles")]
impl<Build, T> Drop for UninstrumentedHandleTask<Build, T> {
    fn drop(&mut self) {
        let panicked = catch_no_unwind(AssertUnwindSafe(|| drop(self.build.take()))).is_err();
        if let Some(responder) = self.responder.take() {
            AsyncDelivery::<()>::new(responder, self.token, None).fallback(panicked);
        }
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
    // Only synchronous launch owns this epoch. Reentrant numeric siblings
    // remain live through admission, user preparation, and call teardown.
    let _numeric = crate::return_abi::numeric::Fp12CallGuard::enter();
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
    let calculation = runtime.async_runtime().snapshot_calculation();
    let calculation_id = calculation.epoch().into();
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
            let delivery = OwnedDeliveryOutcome::Unobserved;
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
        runtime.async_runtime().preflight_snapshot(&calculation)?;
        start(guard, runtime.execution_lease(guard)?, token)
    }))
    .unwrap_or(Err(XllError::Panic));
    match future {
        Ok(future) => {
            let reservation = match runtime.async_runtime().reserve_snapshot(calculation) {
                Ok(reservation) => reservation,
                Err(error) => {
                    let _ = catch_no_unwind(AssertUnwindSafe(|| drop(future)));
                    // SAFETY: the responder owns a copied handle valid for this call.
                    let delivery = match unsafe { responder.deliver_error(&error) } {
                        Ok(()) => OwnedDeliveryOutcome::Delivered,
                        Err(error) => OwnedDeliveryOutcome::Failed(error),
                    };
                    let completion = AsyncCompletion {
                        completion: OwnedCompletionOutcome::Error(error),
                        delivery,
                    };
                    report_completion(udf_id, &completion);
                    observation.finish(&completion);
                    return;
                }
            };
            let task = AsyncDelivery::new(responder, token, Some(observation)).task(future);
            reservation.commit(task, cancellation);
        }
        Err(error) => {
            // SAFETY: the responder owns a copied handle valid for this call.
            let delivery = match unsafe { responder.deliver_error(&error) } {
                Ok(()) => OwnedDeliveryOutcome::Delivered,
                Err(error) => OwnedDeliveryOutcome::Failed(error),
            };
            let completion = AsyncCompletion {
                completion: OwnedCompletionOutcome::Error(error),
                delivery,
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
    let calculation = runtime.async_runtime().snapshot_calculation();
    let (cancellation, token) = CancellationSource::new(CancellationGuarantee::BestEffort);
    // SAFETY: forwarded from this function's raw-handle contract.
    let mut responder = match unsafe { ExcelAsyncResponder::from_raw(udf_id, raw_handle) } {
        Ok(responder) => responder,
        Err(error) => {
            let delivery = OwnedDeliveryOutcome::Unobserved;
            let completion = AsyncCompletion {
                completion: OwnedCompletionOutcome::Error(error),
                delivery,
            };
            report_completion(udf_id, &completion);
            return;
        }
    };
    let future = catch_no_unwind(AssertUnwindSafe(|| {
        runtime.async_runtime().preflight_snapshot(&calculation)?;
        start(guard, runtime.execution_lease(guard)?, token)
    }))
    .unwrap_or(Err(XllError::Panic));
    match future {
        Ok(future) => {
            let reservation = match runtime.async_runtime().reserve_snapshot(calculation) {
                Ok(reservation) => reservation,
                Err(error) => {
                    let _ = catch_no_unwind(AssertUnwindSafe(|| drop(future)));
                    // SAFETY: the responder owns a copied handle valid for this call.
                    let delivery = match unsafe { responder.deliver_error(&error) } {
                        Ok(()) => OwnedDeliveryOutcome::Delivered,
                        Err(error) => OwnedDeliveryOutcome::Failed(error),
                    };
                    let completion = AsyncCompletion {
                        completion: OwnedCompletionOutcome::Error(error),
                        delivery,
                    };
                    report_completion(udf_id, &completion);
                    return;
                }
            };
            let task = AsyncDelivery::<()>::new(responder, token, None).task(future);
            reservation.commit(task, cancellation);
        }
        Err(error) => {
            // SAFETY: the responder owns a copied handle valid for this call.
            let delivery = match unsafe { responder.deliver_error(&error) } {
                Ok(()) => OwnedDeliveryOutcome::Delivered,
                Err(error) => OwnedDeliveryOutcome::Failed(error),
            };
            let completion = AsyncCompletion {
                completion: OwnedCompletionOutcome::Error(error),
                delivery,
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
    // The guard remains on the launch stack; no async task captures it.
    // Keep it through teardown, which can itself reenter a worksheet UDF.
    let _numeric = crate::return_abi::numeric::Fp12CallGuard::enter();
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
    let calculation = runtime.async_runtime().snapshot_calculation();
    let calculation_id = calculation.epoch().into();
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
            let delivery = OwnedDeliveryOutcome::Unobserved;
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
        runtime.async_runtime().preflight_snapshot(&calculation)?;
        let lease = runtime.execution_lease(guard)?;
        let generation = lease.generation();
        start(guard, lease, token).map(|build| (generation, build))
    }))
    .unwrap_or(Err(XllError::Panic));
    match prepared {
        Ok((generation, build)) => {
            let reservation = match runtime.async_runtime().reserve_snapshot(calculation) {
                Ok(reservation) => reservation,
                Err(error) => {
                    let _ = catch_no_unwind(AssertUnwindSafe(|| drop(build)));
                    // SAFETY: the responder owns a copied handle valid for this call.
                    let delivery = match unsafe { responder.deliver_error(&error) } {
                        Ok(()) => OwnedDeliveryOutcome::Delivered,
                        Err(error) => OwnedDeliveryOutcome::Failed(error),
                    };
                    let completion = AsyncCompletion {
                        completion: OwnedCompletionOutcome::Error(error),
                        delivery,
                    };
                    report_completion(udf_id, &completion);
                    observation.finish(&completion);
                    return;
                }
            };
            reservation.commit_handle_scoped(
                generation,
                InstrumentedHandleTask {
                    build: Some(build),
                    responder: Some(responder),
                    token,
                    observation: Some(observation),
                    udf_id,
                    _result: std::marker::PhantomData,
                },
                cancellation,
            );
        }
        Err(error) => {
            // SAFETY: the responder owns a copied handle valid for this call.
            let delivery = match unsafe { responder.deliver_error(&error) } {
                Ok(()) => OwnedDeliveryOutcome::Delivered,
                Err(error) => OwnedDeliveryOutcome::Failed(error),
            };
            let completion = AsyncCompletion {
                completion: OwnedCompletionOutcome::Error(error),
                delivery,
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
    let calculation = runtime.async_runtime().snapshot_calculation();
    let (cancellation, token) = CancellationSource::new(CancellationGuarantee::BestEffort);
    // SAFETY: forwarded from this function's raw-handle contract.
    let mut responder = match unsafe { ExcelAsyncResponder::from_raw(udf_id, raw_handle) } {
        Ok(responder) => responder,
        Err(error) => {
            let delivery = OwnedDeliveryOutcome::Unobserved;
            let completion = AsyncCompletion {
                completion: OwnedCompletionOutcome::Error(error),
                delivery,
            };
            report_completion(udf_id, &completion);
            return;
        }
    };
    let prepared = catch_no_unwind(AssertUnwindSafe(|| {
        runtime.async_runtime().preflight_snapshot(&calculation)?;
        let lease = runtime.execution_lease(guard)?;
        let generation = lease.generation();
        start(guard, lease, token).map(|build| (generation, build))
    }))
    .unwrap_or(Err(XllError::Panic));
    match prepared {
        Ok((generation, build)) => {
            let reservation = match runtime.async_runtime().reserve_snapshot(calculation) {
                Ok(reservation) => reservation,
                Err(error) => {
                    let _ = catch_no_unwind(AssertUnwindSafe(|| drop(build)));
                    // SAFETY: the responder owns a copied handle valid for this call.
                    let delivery = match unsafe { responder.deliver_error(&error) } {
                        Ok(()) => OwnedDeliveryOutcome::Delivered,
                        Err(error) => OwnedDeliveryOutcome::Failed(error),
                    };
                    let completion = AsyncCompletion {
                        completion: OwnedCompletionOutcome::Error(error),
                        delivery,
                    };
                    report_completion(udf_id, &completion);
                    return;
                }
            };
            reservation.commit_handle_scoped(
                generation,
                UninstrumentedHandleTask {
                    build: Some(build),
                    responder: Some(responder),
                    token,
                    udf_id,
                    _result: std::marker::PhantomData,
                },
                cancellation,
            );
        }
        Err(error) => {
            // SAFETY: the responder owns a copied handle valid for this call.
            let delivery = match unsafe { responder.deliver_error(&error) } {
                Ok(()) => OwnedDeliveryOutcome::Delivered,
                Err(error) => OwnedDeliveryOutcome::Failed(error),
            };
            let completion = AsyncCompletion {
                completion: OwnedCompletionOutcome::Error(error),
                delivery,
            };
            report_completion(udf_id, &completion);
        }
    }
}

pub(super) fn report_completion(udf_id: &'static str, completion: &AsyncCompletion) {
    if let OwnedCompletionOutcome::Error(error) = &completion.completion {
        crate::diagnostics::report_udf_failure_no_unwind(udf_id, error);
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

#[cfg(test)]
mod numeric_launch_tests {
    use super::*;
    use crate::return_abi::numeric::{NumericArrayOutput, numeric_return_memory_metrics};
    use std::cell::Cell;
    use xlfn_sys::{FP12, XLOPER12BigData, XLOPER12BigDataHandle, XLOPER12Value, XLTYPE_BIG_DATA};

    struct LaunchTestAddin;

    impl crate::Addin for LaunchTestAddin {
        type SharedState = ();
        type LifecycleState = ();
        type Error = XllError;
        type Layers = ();
        type AsyncExecutor = crate::NoAsyncExecutor;

        fn open(_: &crate::OpenContext) -> crate::OpenResult<Self> {
            unreachable!()
        }
    }

    unsafe fn first_number(pointer: *mut FP12) -> f64 {
        // SAFETY: each caller retains the current published epoch, whose
        // initialized 1x1 FP12 payload begins after the eight-byte ABI header.
        let payload = unsafe { pointer.cast::<f64>().add(1) };
        // SAFETY: the selected numeric payload is initialized and still live.
        unsafe { payload.read() }
    }

    fn numeric_siblings(runtime: &'static Runtime<LaunchTestAddin>) -> *mut FP12 {
        let first = crate::return_abi::udf_boundary_numeric_named(
            runtime,
            "async_launch_first_numeric",
            "ASYNC.LAUNCH.FIRST.NUMERIC",
            |_, _| NumericArrayOutput::from_f64(1, 1, &[1.0]),
        );
        let second = crate::return_abi::udf_boundary_numeric_named(
            runtime,
            "async_launch_second_numeric",
            "ASYNC.LAUNCH.SECOND.NUMERIC",
            |_, _| NumericArrayOutput::from_f64(1, 1, &[2.0]),
        );
        assert!(!first.is_null());
        assert!(!second.is_null());
        assert_eq!(numeric_return_memory_metrics().thread_published_buffers, 2);
        // SAFETY: sibling launch calls share the still-active outer epoch.
        assert_eq!(unsafe { first_number(first) }, 1.0);
        first
    }

    fn verify_launch(
        runtime: &'static Runtime<LaunchTestAddin>,
        launch: impl FnOnce(&'static Runtime<LaunchTestAddin>, *mut XLOPER12, &Cell<*mut FP12>),
    ) {
        let _test = crate::runtime::tests::TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let fixture = crate::runtime::StaticTestRuntime::from_static(runtime);
        let runtime = fixture.runtime();
        let open = runtime.begin_open().unwrap();
        let mut open = runtime.publish(open, (), ());
        runtime.finish_open(&mut open, Vec::new()).unwrap();
        runtime
            .start_async(crate::NoAsyncExecutor, crate::AsyncTaskLimit::DEFAULT)
            .unwrap();
        let _callback = crate::test_callback::lock();
        crate::test_callback::install();
        crate::test_callback::reset();
        let mut handle = XLOPER12 {
            value: XLOPER12Value {
                big_data: XLOPER12BigData {
                    handle: XLOPER12BigDataHandle {
                        data: std::ptr::null_mut(),
                    },
                    byte_count: 0,
                },
            },
            xltype: XLTYPE_BIG_DATA,
        };
        let first = Cell::new(std::ptr::null_mut());
        launch(runtime, &mut handle, &first);
        assert!(!first.get().is_null());
        assert_eq!(crate::test_callback::async_return_calls(), 1);
        assert_eq!(numeric_return_memory_metrics().thread_published_buffers, 2);
        // SAFETY: return from launch, including runtime guard teardown, must
        // retain both numeric results until the next outer worksheet entry.
        assert_eq!(unsafe { first_number(first.get()) }, 1.0);
        assert!(runtime.close_async().unwrap().issues.is_empty());
        drop(crate::return_abi::numeric::Fp12CallGuard::enter());
    }

    #[test]
    fn miri_async_launch_retains_nested_numeric_siblings() {
        static RUNTIME: Runtime<LaunchTestAddin> = Runtime::new();
        verify_launch(&RUNTIME, |runtime, handle, first| {
            // SAFETY: verify_launch owns this aligned, initialized async token
            // for the entire synchronous launch call.
            unsafe {
                async_udf_boundary_named(
                    runtime,
                    "async_launch",
                    "ASYNC.LAUNCH",
                    handle,
                    |_, _, _| {
                        first.set(numeric_siblings(runtime));
                        Err::<std::future::Ready<XllResult<f64>>, _>(XllError::Overloaded)
                    },
                );
            }
        });
    }

    #[cfg(feature = "handles")]
    struct UnusedHandleBuilder;

    #[cfg(feature = "handles")]
    impl HandleScopedBuilder<f64> for UnusedHandleBuilder {
        fn build<'generation>(
            self,
            _: super::super::task_scope::AsyncTaskScope<'generation>,
        ) -> ScopedTaskFuture<'generation, XllResult<f64>> {
            unreachable!("launch returns an error before building any async task")
        }
    }

    #[cfg(feature = "handles")]
    #[test]
    fn miri_handle_async_launch_retains_nested_numeric_siblings() {
        static RUNTIME: Runtime<LaunchTestAddin> = Runtime::new();
        verify_launch(&RUNTIME, |runtime, handle, first| {
            // SAFETY: verify_launch owns this aligned, initialized async token
            // for the entire synchronous launch call.
            unsafe {
                async_udf_boundary_named_handle(
                    runtime,
                    "handle_async_launch",
                    "HANDLE.ASYNC.LAUNCH",
                    handle,
                    |_, _, _| {
                        first.set(numeric_siblings(runtime));
                        Err::<UnusedHandleBuilder, _>(XllError::Overloaded)
                    },
                );
            }
        });
    }
}
