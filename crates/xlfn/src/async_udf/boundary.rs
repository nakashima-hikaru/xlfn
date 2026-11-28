use super::completion::{
    AsyncCompletion, AsyncDiagnosticContext, OwnedCompletionOutcome, OwnedDeliveryOutcome,
    PendingDelivery, return_error,
};
use super::instrumentation::{AsyncObservation, CompletionObserver, NoObservation};
#[cfg(feature = "handles")]
use super::task_scope::HandleScopedBuilder;
use super::transport::ExcelAsyncResponder;
use crate::call_return::ExcelReturn;
use crate::cancellation::{CancellationGuarantee, CancellationToken};
mod launch;
#[cfg(all(test, feature = "handles", feature = "async-builtin"))]
use crate::execution::CallMetadata;
use crate::execution::{CallId, InstrumentationPlan};
use crate::panic_boundary::catch_no_unwind;
use crate::runtime::Runtime;
#[cfg(test)]
use crate::sync::Mutex;
use crate::{XllError, XllResult};
use futures_util::Future;
#[cfg(feature = "handles")]
use launch::ScopedStart;
use launch::{AsyncStart, CapturedLaunch, LaunchOwner, PlainStart};
use std::panic::AssertUnwindSafe;
use xlfn_sys::XLOPER12;

#[cfg(all(test, feature = "handles", feature = "async-builtin"))]
mod scoped_tests;

/// Runs the synchronous launch portion of a native Excel async UDF.
///
/// # Safety
///
/// `raw_handle` must point to a valid, aligned, Excel-owned `XLOPER12` async
/// handle that remains live for the duration of this call.
#[doc(hidden)]
pub(crate) unsafe fn async_udf_boundary_named<A, Start, Fut, T>(
    runtime: &'static Runtime<A, impl crate::runtime::UnloadPolicy<A>>,
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
    T: ExcelReturn<InputMode = crate::value::PlainInputMode> + Send + 'static,
{
    // SAFETY: forwarded from this function's raw-handle contract.
    unsafe { run_async_boundary(runtime, udf_id, excel_name, raw_handle, PlainStart(start)) };
}

unsafe fn run_async_boundary<A: crate::Addin>(
    runtime: &'static Runtime<A, impl crate::runtime::UnloadPolicy<A>>,
    udf_id: &'static str,
    excel_name: &'static str,
    raw_handle: *mut XLOPER12,
    start: impl AsyncStart<A>,
) {
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

    let call = match runtime.enter_async(&ingress) {
        Ok(call) => call,
        Err(error) => {
            // SAFETY: the caller keeps the raw handle live, and ingress is
            // retained until this rejected launch has finished its callback.
            unsafe {
                reject_async_admission(&ingress, runtime.diagnostics(), udf_id, raw_handle, error)
            };
            return;
        }
    };

    let result = catch_no_unwind(AssertUnwindSafe(|| {
        // SAFETY: forwarded from this function's raw-handle contract.
        unsafe {
            run_admitted_async(&call, udf_id, excel_name, raw_handle, start);
        }
    }));

    if result.is_err() {
        crate::diagnostics::report_no_unwind(udf_id, &XllError::PANIC);
    }

    drop(call);
}

/// Completes a launch rejected after module entry but before generation
/// admission. Closing can win between these two admissions, before an owned
/// responder exists to supply its usual fallback completion.
unsafe fn reject_async_admission(
    ingress: &crate::ingress::AdmittedExport<'_>,
    history: &'static crate::diagnostics::DiagnosticHistory,
    udf_id: &'static str,
    raw_handle: *mut XLOPER12,
    error: XllError,
) {
    let result = catch_no_unwind(AssertUnwindSafe(|| {
        ingress.assert_active();
        // SAFETY: the rejected launch retains both the caller's live raw
        // handle and module ingress. return_error validates the handle and
        // separately acquires callback admission before invoking Excel.
        let delivery = unsafe { return_error(raw_handle, &error) };
        report_completion(
            Some(AsyncDiagnosticContext {
                history,
                call_id: None,
                calculation_id: None,
            }),
            udf_id,
            &AsyncCompletion {
                completion: OwnedCompletionOutcome::Error(error),
                delivery,
            },
        );
    }));
    if result.is_err() {
        crate::diagnostics::report_no_unwind(udf_id, &XllError::PANIC);
    }
}

#[cfg(all(test, feature = "async-builtin"))]
pub(crate) unsafe fn async_udf_boundary_named_inner<A, Start, Fut, T>(
    guard: &crate::runtime::AsyncCallGuard<'_, A>,
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
    T: ExcelReturn<InputMode = crate::value::PlainInputMode> + Send + 'static,
{
    // SAFETY: forwarded from this test entry point's raw-handle contract.
    unsafe { run_admitted_async(guard, udf_id, excel_name, raw_handle, PlainStart(start)) };
}

unsafe fn run_admitted_async<A: crate::Addin>(
    guard: &crate::runtime::AsyncCallGuard<'_, A>,
    udf_id: &'static str,
    excel_name: &'static str,
    raw_handle: *mut XLOPER12,
    start: impl AsyncStart<A>,
) {
    let instrumentation = InstrumentationPlan::for_call(guard);
    if !instrumentation.enabled() {
        let launch = CapturedLaunch::capture(guard).into_launch();
        let diagnostics = AsyncDiagnosticContext {
            history: guard.history(),
            call_id: None,
            calculation_id: Some(launch.calculation_id()),
        };
        // SAFETY: forwarded from this function's raw-handle contract.
        unsafe {
            prepare_async_launch(
                launch,
                udf_id,
                raw_handle,
                start,
                NoObservation,
                Some(diagnostics),
            )
        };
        return;
    }
    let call_id = CallId::new(guard.next_call_id());
    let (entry, captured) = instrumentation.start(udf_id, excel_name, call_id, || {
        let captured = CapturedLaunch::capture(guard);
        (captured.calculation_id(), captured)
    });
    let diagnostics = AsyncDiagnosticContext {
        history: guard.history(),
        call_id: Some(call_id),
        calculation_id: Some(captured.calculation_id()),
    };
    let observed = match entry.enter(guard.layers(), |error, timer| {
        // SAFETY: forwarded from this function's raw-handle contract.
        let delivery = unsafe { return_error(raw_handle, &error) };
        (
            AsyncCompletion {
                completion: OwnedCompletionOutcome::Error(error),
                delivery,
            },
            timer.elapsed(),
        )
    }) {
        Ok(observed) => observed,
        Err(rejection) => {
            rejection.finish(|(completion, _)| {
                report_completion(Some(diagnostics), udf_id, completion);
            });
            return;
        }
    };
    // The issuer was captured before any user layer ran. Allocate cancellation
    // ownership only after successful entry. Excel events still do not scope
    // every programmatic calculation, so the public token is best-effort.
    let launch = captured.into_launch();
    let observation = AsyncObservation::from_call(observed, launch.token());
    // SAFETY: forwarded from this function's raw-handle contract.
    unsafe {
        prepare_async_launch(
            launch,
            udf_id,
            raw_handle,
            start,
            observation,
            Some(diagnostics),
        )
    };
}

unsafe fn prepare_async_launch<A: crate::Addin, S: AsyncStart<A>>(
    launch: LaunchOwner<'_, '_, A>,
    udf_id: &'static str,
    raw_handle: *mut XLOPER12,
    start: S,
    observation: impl CompletionObserver + Send + 'static,
    diagnostics: Option<AsyncDiagnosticContext>,
) {
    // SAFETY: forwarded from this function's raw-handle contract.
    let responder = match unsafe { ExcelAsyncResponder::from_raw(udf_id, raw_handle) } {
        Ok(responder) => responder,
        Err(error) => {
            let completion = AsyncCompletion {
                completion: OwnedCompletionOutcome::Error(error),
                delivery: OwnedDeliveryOutcome::Unobserved,
            };
            report_completion(diagnostics, udf_id, &completion);
            observation.finish(&completion);
            return;
        }
    };
    if let Some(reservation) = launch
        .prepare(start, responder, observation, diagnostics)
        .and_then(launch::PreparedLaunch::reserve)
    {
        S::commit(reservation);
    }
}

/// Specialized async-UDF boundary for generation-scoped `HandleLease`
/// parameters. The start closure decodes owned pending pins during the Excel
/// call; the builder brands those pins only after spawn admission succeeds.
#[doc(hidden)]
#[cfg(feature = "handles")]
pub(crate) unsafe fn async_udf_boundary_named_handle<A, Start, Build, T>(
    runtime: &'static Runtime<A, impl crate::runtime::UnloadPolicy<A>>,
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
    T: ExcelReturn<InputMode = crate::value::PlainInputMode> + Send + 'static,
{
    // SAFETY: forwarded from this function's raw-handle contract.
    unsafe {
        run_async_boundary(
            runtime,
            udf_id,
            excel_name,
            raw_handle,
            ScopedStart::<_, T>::new(start),
        )
    };
}

pub(super) fn report_panic(
    diagnostics: Option<AsyncDiagnosticContext>,
    udf_id: &'static str,
    message: &str,
) {
    if let Some(context) = diagnostics {
        crate::diagnostics::report_udf_panic_with_history_no_unwind(
            context.history,
            udf_id,
            context.call_id,
            context.calculation_id,
            message,
        );
    } else {
        crate::diagnostics::report_no_unwind(udf_id, &XllError::PANIC);
    }
}

pub(super) fn report_completion(
    diagnostics: Option<AsyncDiagnosticContext>,
    udf_id: &'static str,
    completion: &AsyncCompletion,
) {
    match &completion.completion {
        OwnedCompletionOutcome::Error(error) => {
            if let Some(context) = diagnostics {
                crate::diagnostics::report_udf_failure_with_history_no_unwind(
                    context.history,
                    udf_id,
                    context.call_id,
                    context.calculation_id,
                    error,
                    false,
                );
            } else {
                crate::diagnostics::report_udf_failure_no_unwind(udf_id, error);
            }
        }
        OwnedCompletionOutcome::Panic(message) => report_panic(diagnostics, udf_id, message),
        OwnedCompletionOutcome::Success | OwnedCompletionOutcome::Cancelled => {}
    }
    if let OwnedDeliveryOutcome::Failed(error) = &completion.delivery {
        if let Some(context) = diagnostics {
            crate::diagnostics::report_udf_failure_with_history_no_unwind(
                context.history,
                udf_id,
                context.call_id,
                context.calculation_id,
                error,
                true,
            );
        } else {
            crate::diagnostics::report_no_unwind(udf_id, error);
        }
    }
}
#[cfg(test)]
pub(crate) static AFTER_ASYNC_EVALUATION_HOOK: Mutex<Option<fn()>> = Mutex::new(None);

pub(crate) fn cancel_async_calculation<A: crate::Addin>(
    runtime: &Runtime<A, impl crate::runtime::UnloadPolicy<A>>,
) {
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
        #[cfg(feature = "rtd")]
        type RtdSubscription = ();
        #[cfg(feature = "rtd")]
        type RtdSources = crate::DynamicRtdSources<Self::RtdSubscription>;
        type SharedState = ();
        type LifecycleState = ();
        type Error = XllError;
        type Layers = ();
        type AsyncExecutor = crate::NoAsyncExecutor;

        fn open(_: &crate::OpenContext<Self>) -> crate::OpenResult<Self> {
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
        assert_eq!(
            numeric_return_memory_metrics()
                .unwrap()
                .thread_published_buffers,
            2
        );
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
        let open = runtime.publish(open, (), ());
        runtime.finish_open(open, Vec::new()).unwrap();
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
        assert_eq!(
            numeric_return_memory_metrics()
                .unwrap()
                .thread_published_buffers,
            2
        );
        // SAFETY: return from launch, including runtime guard teardown, must
        // retain both numeric results until the next outer worksheet entry.
        assert_eq!(unsafe { first_number(first.get()) }, 1.0);
        assert!(runtime.close_async().unwrap().issues.is_empty());
        drop(crate::return_abi::numeric::Fp12CallGuard::enter());
    }

    fn verify_rejected_admission(
        runtime: &'static Runtime<LaunchTestAddin>,
        launch: impl Fn(&'static Runtime<LaunchTestAddin>, *mut XLOPER12),
    ) {
        let _test = crate::runtime::tests::TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let fixture = crate::runtime::StaticTestRuntime::from_static(runtime);
        let runtime = fixture.runtime();
        let open = runtime.begin_open().unwrap();
        let open = runtime.publish(open, (), ());
        runtime.finish_open(open, Vec::new()).unwrap();
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

        // Stop at the real removal ordering between closing the generation
        // and closing module ingress. The boundary can still enter the DLL,
        // but it cannot acquire an AsyncCallGuard or create a responder.
        runtime.close_generation_admission_for_test();
        let ingress = crate::module_runtime::ingress();
        assert_eq!(ingress.phase(), crate::ingress::PHASE_OPEN);
        launch(runtime, &mut handle);
        assert_eq!(crate::test_callback::async_return_calls(), 1);
        assert_eq!(crate::test_callback::last_async_value(), -1);

        // Rejection must still validate the raw handle, then honor callback
        // closure; neither path may invoke Excel merely to return an error.
        let mut invalid = XLOPER12::default();
        launch(runtime, &mut invalid);
        assert_eq!(crate::test_callback::async_return_calls(), 1);
        crate::module_runtime::close_callbacks_for_test();
        launch(runtime, &mut handle);
        assert_eq!(crate::test_callback::async_return_calls(), 1);

        ingress.begin_close_with(|| {});
        assert!(ingress.seal_and_drain().is_ok());
        launch(runtime, &mut handle);
        assert_eq!(crate::test_callback::async_return_calls(), 1);
        assert_eq!(ingress.active_udfs(), 0);
    }

    #[test]
    fn miri_async_admission_rejection_completes_only_while_callbacks_are_open() {
        static RUNTIME: Runtime<LaunchTestAddin> = Runtime::new();
        verify_rejected_admission(&RUNTIME, |runtime, handle| {
            // SAFETY: the fixture retains aligned, initialized raw storage
            // through launch, including the intentionally invalid tag case.
            unsafe {
                async_udf_boundary_named(
                    runtime,
                    "rejected_async_admission",
                    "REJECTED.ASYNC.ADMISSION",
                    handle,
                    |_, _, _| -> XllResult<std::future::Ready<XllResult<f64>>> {
                        panic!("rejected generation must not run user preparation");
                    },
                );
            }
        });
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
                        Err::<std::future::Ready<XllResult<f64>>, _>(XllError::OVERLOADED)
                    },
                );
            }
        });
    }

    #[test]
    fn miri_launch_prepare_panic_keeps_its_responder_and_numeric_epoch() {
        static RUNTIME: Runtime<LaunchTestAddin> = Runtime::new();
        verify_launch(&RUNTIME, |runtime, handle, first| {
            // SAFETY: verify_launch owns this initialized async token through
            // the complete synchronous launch and its recovery callback.
            unsafe {
                async_udf_boundary_named(
                    runtime,
                    "prepare_panic",
                    "PREPARE.PANIC",
                    handle,
                    |_, _, _| -> XllResult<std::future::Ready<XllResult<f64>>> {
                        first.set(numeric_siblings(runtime));
                        panic!("injected user preparation panic");
                    },
                );
            }
        });
    }

    #[test]
    fn miri_launch_reservation_failure_uses_the_retained_responder_once() {
        static RUNTIME: Runtime<LaunchTestAddin> = Runtime::new();
        verify_launch(&RUNTIME, |runtime, handle, first| {
            // SAFETY: verify_launch retains this initialized async token. Its
            // NoAsyncExecutor rejects reservation after preparation succeeds.
            unsafe {
                async_udf_boundary_named(
                    runtime,
                    "reserve_failure",
                    "RESERVE.FAILURE",
                    handle,
                    |_, _, _| {
                        first.set(numeric_siblings(runtime));
                        Ok(std::future::ready(Ok::<f64, XllError>(7.0)))
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
        ) -> impl Future<Output = XllResult<f64>> + Send + 'generation {
            std::future::poll_fn(|_| {
                unreachable!("launch returns an error before building any async task")
            })
        }
    }

    #[cfg(feature = "handles")]
    #[test]
    fn miri_handle_async_admission_rejection_completes_only_while_callbacks_are_open() {
        static RUNTIME: Runtime<LaunchTestAddin> = Runtime::new();
        verify_rejected_admission(&RUNTIME, |runtime, handle| {
            // SAFETY: the fixture owns initialized raw storage throughout
            // launch; the boundary validates its tag before returning it.
            unsafe {
                async_udf_boundary_named_handle(
                    runtime,
                    "rejected_handle_async_admission",
                    "REJECTED.HANDLE.ASYNC.ADMISSION",
                    handle,
                    |_, _, _| -> XllResult<UnusedHandleBuilder> {
                        panic!("rejected generation must not prepare handle pins");
                    },
                );
            }
        });
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
                        Err::<UnusedHandleBuilder, _>(XllError::OVERLOADED)
                    },
                );
            }
        });
    }
}
