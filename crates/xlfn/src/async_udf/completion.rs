use super::instrumentation::{CompletionObserver, NoObservation};
use super::transport::ExcelAsyncResponder;
use crate::call_return::{ExcelReturn, ReturnContext};
use crate::cancellation::CancellationToken;
use crate::error::ExcelCallbackStatus;
use crate::panic_boundary::{CaughtPanic, catch_no_unwind};
use crate::return_abi::AsyncReturnValue;
use crate::{XllError, XllResult};
use futures_util::Future;
use std::panic::AssertUnwindSafe;
use std::ptr::NonNull;
use xlfn_kernel::rollback::{Rollback, RollbackGuard};
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

/// One delivery attempt paired with its completion observer. Abandoning this
/// capability attempts the fallback and reports its outcome to the observer.
pub(crate) struct PendingDelivery<O: CompletionObserver = NoObservation> {
    owner: RollbackGuard<DeliveryParts<O>>,
}

struct DeliveryParts<O: CompletionObserver> {
    responder: ExcelAsyncResponder,
    token: CancellationToken,
    observation: O,
}

impl<O: CompletionObserver> PendingDelivery<O> {
    pub(crate) fn new(
        responder: ExcelAsyncResponder,
        token: CancellationToken,
        observation: O,
    ) -> Self {
        Self {
            owner: RollbackGuard::new(DeliveryParts {
                responder,
                token,
                observation,
            }),
        }
    }

    pub(crate) fn task<Fut, T>(self, future: Fut) -> AsyncUdfTask<Fut, O>
    where
        Fut: Future<Output = XllResult<T>> + Send,
        T: ExcelReturn<InputMode = crate::value::PlainInputMode> + Send + 'static,
    {
        AsyncUdfTask {
            future: super::future::NoUnwindFuture::new(self.owner.get().responder.udf_id(), future),
            delivery: DeliverySlot::Pending(self),
        }
    }

    /// Completes a launch failure using the responder retained before user
    /// preparation. Consuming the capability disarms fallback before reentry.
    pub(super) fn fail(self, error: XllError) {
        self.owner.complete().fail(error);
    }

    pub(super) fn fallback(self, panicked: bool) {
        self.owner.complete().fallback(panicked);
    }

    fn complete<T>(self, evaluated: Result<XllResult<T>, CaughtPanic>)
    where
        T: ExcelReturn<InputMode = crate::value::PlainInputMode> + Send + 'static,
    {
        self.owner.complete().complete(evaluated);
    }
}

impl<O: CompletionObserver> DeliveryParts<O> {
    fn finish(observation: O, udf_id: &'static str, completion: AsyncCompletion) {
        super::boundary::report_completion(udf_id, &completion);
        observation.finish(&completion);
    }

    fn fail(self, error: XllError) {
        let Self {
            responder,
            observation,
            ..
        } = self;
        let udf_id = responder.udf_id();
        let delivery = deliver_error(responder, &error);
        Self::finish(
            observation,
            udf_id,
            AsyncCompletion {
                completion: OwnedCompletionOutcome::Error(error),
                delivery,
            },
        );
    }

    fn fallback(self, panicked: bool) {
        let Self {
            responder,
            token,
            observation,
        } = self;
        let udf_id = responder.udf_id();
        let cancelled = token.is_cancelled();
        if cancelled && panicked {
            crate::diagnostics::report_no_unwind(udf_id, &XllError::Panic);
        }
        let completion = if cancelled {
            OwnedCompletionOutcome::Cancelled
        } else if panicked {
            OwnedCompletionOutcome::Error(XllError::Panic)
        } else {
            OwnedCompletionOutcome::Error(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::ASYNC_COMPLETION,
            })
        };
        let error = if panicked && !cancelled {
            XllError::Panic
        } else {
            XllError::ExcelValue(crate::ExcelError::NotAvailable)
        };
        let delivery = deliver_error(responder, &error);
        Self::finish(
            observation,
            udf_id,
            AsyncCompletion {
                completion,
                delivery,
            },
        );
    }

    fn complete<T>(self, evaluated: Result<XllResult<T>, CaughtPanic>)
    where
        T: ExcelReturn<InputMode = crate::value::PlainInputMode> + Send + 'static,
    {
        let Self {
            responder,
            token,
            observation,
        } = self;
        let udf_id = responder.udf_id();
        let completion = complete_evaluation(responder, token, evaluated);
        Self::finish(observation, udf_id, completion);
    }
}

impl<O: CompletionObserver> Rollback for DeliveryParts<O> {
    fn rollback(self) {
        self.fallback(false);
    }
}

enum DeliverySlot<O: CompletionObserver> {
    Pending(PendingDelivery<O>),
    // Consumed before attempting delivery, including on callback failure.
    Taken,
}

impl<O: CompletionObserver> DeliverySlot<O> {
    fn take(&mut self) -> Option<PendingDelivery<O>> {
        match std::mem::replace(self, Self::Taken) {
            Self::Pending(delivery) => Some(delivery),
            Self::Taken => None,
        }
    }
}

/// Explicit fields enforce cleanup order even when the task is never polled.
/// User destruction is contained, and precedes Excel delivery and layer exit.
pub(crate) struct AsyncUdfTask<Fut, O: CompletionObserver> {
    future: super::future::NoUnwindFuture<Fut>,
    delivery: DeliverySlot<O>,
}

impl<Fut, O: CompletionObserver> Drop for AsyncUdfTask<Fut, O> {
    fn drop(&mut self) {
        // SAFETY: the field is destroyed in place and never moved after polling.
        let panicked = unsafe { std::pin::Pin::new_unchecked(&mut self.future) }
            .finish()
            .is_err();
        if let Some(delivery) = self.delivery.take() {
            delivery.fallback(panicked);
        }
    }
}

impl<Fut, T, O> Future for AsyncUdfTask<Fut, O>
where
    Fut: Future<Output = XllResult<T>> + Send,
    T: ExcelReturn<InputMode = crate::value::PlainInputMode> + Send + 'static,
    O: CompletionObserver,
{
    type Output = ();

    fn poll(
        self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<()> {
        // SAFETY: only the user future is pinned; it is never moved and Drop
        // finishes it in place before accessing delivery state.
        let this = unsafe { self.get_unchecked_mut() };
        // SAFETY: the enclosing pin keeps this field fixed until in-place
        // destruction; the projection never moves or replaces it.
        let mut future = unsafe { std::pin::Pin::new_unchecked(&mut this.future) };
        let evaluated = match catch_no_unwind(AssertUnwindSafe(|| future.as_mut().poll(context))) {
            Ok(std::task::Poll::Pending) => return std::task::Poll::Pending,
            Ok(std::task::Poll::Ready(value)) => Ok(value),
            Err(panic) => Err(panic),
        };
        let finish = future.finish();
        let Some(delivery) = this.delivery.take() else {
            xlfn_kernel::invariant::fail_stop();
        };
        let evaluated = match finish {
            Ok(()) => evaluated,
            Err(panic) => {
                drop_evaluation_no_unwind(delivery.owner.get().responder.udf_id(), evaluated);
                Err(panic)
            }
        };
        #[cfg(test)]
        if let Some(hook) = *super::boundary::AFTER_ASYNC_EVALUATION_HOOK.lock() {
            hook();
        }
        delivery.complete(evaluated);
        std::task::Poll::Ready(())
    }
}

fn deliver_error(responder: ExcelAsyncResponder, error: &XllError) -> OwnedDeliveryOutcome {
    // SAFETY: the responder owns its validated, copied Excel handle.
    match unsafe { responder.deliver_error(error) } {
        Ok(()) => OwnedDeliveryOutcome::Delivered,
        Err(error) => OwnedDeliveryOutcome::Failed(error),
    }
}

fn drop_evaluation_no_unwind<T>(
    udf_id: &'static str,
    evaluated: Result<XllResult<T>, CaughtPanic>,
) {
    if catch_no_unwind(AssertUnwindSafe(|| drop(evaluated))).is_err() {
        crate::diagnostics::report_no_unwind(udf_id, &XllError::Panic);
    }
}

fn complete_evaluation<
    T: ExcelReturn<InputMode = crate::value::PlainInputMode> + Send + 'static,
>(
    responder: ExcelAsyncResponder,
    token: CancellationToken,
    evaluated: Result<XllResult<T>, CaughtPanic>,
) -> AsyncCompletion {
    if !token.try_start_delivery() {
        // Cancellation owns the worksheet outcome, not the panic diagnostic.
        if evaluated.is_err() {
            crate::diagnostics::report_no_unwind(responder.udf_id(), &XllError::Panic);
        }
        drop_evaluation_no_unwind(responder.udf_id(), evaluated);
        let cancel_error = XllError::ExcelValue(crate::ExcelError::NotAvailable);
        return AsyncCompletion {
            completion: OwnedCompletionOutcome::Cancelled,
            delivery: deliver_error(responder, &cancel_error),
        };
    }
    let (pointer, completion) = match evaluated {
        Ok(Ok(value)) => {
            let result = catch_no_unwind(AssertUnwindSafe(|| {
                let mut return_context = ReturnContext::new();
                let value = T::invoke(&mut return_context, || Ok(value))?;
                AsyncReturnValue::from_value(value)
            }))
            .unwrap_or(Err(XllError::Panic));
            match result {
                Ok(pointer) => (pointer, OwnedCompletionOutcome::Success),
                Err(error) => (
                    AsyncReturnValue::error(&error),
                    OwnedCompletionOutcome::Error(error),
                ),
            }
        }
        Ok(Err(error)) => (
            AsyncReturnValue::error(&error),
            OwnedCompletionOutcome::Error(error),
        ),
        Err(_) => {
            let error = XllError::Panic;
            (
                AsyncReturnValue::error(&error),
                OwnedCompletionOutcome::Error(error),
            )
        }
    };
    // SAFETY: the responder owns the handle and marks the attempt before FFI.
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

/// Validates a raw async handle and attempts to deliver an error through it.
pub(crate) unsafe fn return_error(handle: *mut XLOPER12, error: &XllError) -> OwnedDeliveryOutcome {
    // Rejected raw operands must never be forwarded back into Excel.
    // SAFETY: forwarded from the caller's live-storage contract.
    if unsafe { ExcelAsyncResponder::validate_raw(handle) }.is_err() {
        return OwnedDeliveryOutcome::Unobserved;
    }
    let Some(handle) = NonNull::new(handle) else {
        return OwnedDeliveryOutcome::Unobserved;
    };
    let mut value = AsyncReturnValue::error(error);
    // SAFETY: the raw handle is valid under the caller's FFI contract and the
    // return pointer remains owned until async_return returns.
    match unsafe { async_return(handle, NonNull::from_mut(value.as_raw())) } {
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
    drop(callback_admission);
    check_async_acceptance(
        ExcelCallbackStatus::from_raw(raw_status),
        callback_result,
        invoked,
    )
}

fn check_async_acceptance(
    status: ExcelCallbackStatus,
    callback_result: XLOPER12,
    invoked: bool,
) -> XllResult<()> {
    let accepted = invoked
        && status == ExcelCallbackStatus::Success
        && callback_result.base_type() == XLTYPE_BOOL
        // SAFETY: XLTYPE_BOOL selects the boolean union field.
        && unsafe { callback_result.value.boolean != 0 };
    if !accepted {
        let failure = if status == ExcelCallbackStatus::Success {
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
mod tests {
    #[test]
    fn unobserved_task_layout_omits_instrumentation_storage() {
        type Pending = std::future::Pending<crate::XllResult<()>>;
        type Unobserved = super::AsyncUdfTask<Pending, super::NoObservation>;
        type Observed =
            super::AsyncUdfTask<Pending, crate::async_udf::instrumentation::AsyncObservation<()>>;
        assert_eq!(std::mem::size_of::<super::NoObservation>(), 0);
        assert!(std::mem::size_of::<Unobserved>() < std::mem::size_of::<Observed>());
        eprintln!(
            "GENERIC_LAYOUT delivery_unobserved={} task_unobserved={} tracked_task={} panic_wrapper_dynamic={} panic_wrapper_static={}",
            std::mem::size_of::<super::PendingDelivery<super::NoObservation>>(),
            std::mem::size_of::<Unobserved>(),
            std::mem::size_of::<crate::async_udf::task::TrackedFuture<Unobserved>>(),
            std::mem::size_of::<crate::async_udf::future::NoUnwindFuture<std::future::Pending<()>>>(
            ),
            std::mem::size_of::<
                crate::async_udf::future::NoUnwindFuture<
                    std::future::Pending<()>,
                    crate::async_udf::future::TaskDestruction,
                >,
            >(),
        );
    }

    use super::*;
    use crate::cancellation::{CancellationGuarantee, CancellationSource};
    use crate::execution::{
        CalculationId, CallId, CallMetadata, CallTimer, UdfCompletionOutcome, UdfDeliveryOutcome,
        UdfLayerGuard,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::task::{Context, Poll};
    use xlfn_sys::{XLOPER12BigData, XLOPER12BigDataHandle, XLOPER12Value, XLTYPE_BIG_DATA};

    #[test]
    fn uninvoked_async_return_preserves_the_callback_failure_status() {
        use crate::error::{ExcelApiFailure, ExcelApiFunction};
        let failed = ExcelCallbackStatus::from_raw(xlfn_sys::XLRET_FAILED);
        assert!(matches!(
            check_async_acceptance(failed, XLOPER12::nil(), false),
            Err(XllError::ExcelApi {
                function: ExcelApiFunction::AsyncReturn,
                failure: ExcelApiFailure::Status(status),
            }) if status == failed
        ));
        assert!(matches!(
            check_async_acceptance(ExcelCallbackStatus::Success, XLOPER12::boolean(false), true),
            Err(XllError::ExcelApi {
                function: ExcelApiFunction::AsyncReturn,
                failure: ExcelApiFailure::UnexpectedResult,
            })
        ));
    }

    #[test]
    fn cancelled_async_panic_is_reported_without_changing_the_worksheet_outcome() {
        struct DiagnosticCounter(Arc<AtomicUsize>);
        impl tracing::Subscriber for DiagnosticCounter {
            fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
                true
            }
            fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
                tracing::span::Id::from_u64(1)
            }
            fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
            fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
            fn event(&self, event: &tracing::Event<'_>) {
                if event.metadata().fields().field("diagnostic_id").is_some() {
                    self.0.fetch_add(1, Ordering::Relaxed);
                }
            }
            fn enter(&self, _: &tracing::span::Id) {}
            fn exit(&self, _: &tracing::span::Id) {}
        }

        let _runtime = crate::runtime::tests::TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _callbacks = crate::test_callback::lock();
        crate::test_callback::install();
        crate::test_callback::reset();
        let reports = Arc::new(AtomicUsize::new(0));
        let dispatch = tracing::Dispatch::new(DiagnosticCounter(Arc::clone(&reports)));
        let mut raw = XLOPER12 {
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
        // SAFETY: the responder copies the live, initialized handle.
        let responder = unsafe { ExcelAsyncResponder::from_raw("CANCEL.PANIC", &mut raw).unwrap() };
        let (source, token) = CancellationSource::new(CancellationGuarantee::BestEffort);
        let evaluated =
            catch_no_unwind(|| -> XllResult<f64> { panic!("injected async evaluation panic") });
        source.cancel();
        let completion = tracing::dispatcher::with_default(&dispatch, || {
            complete_evaluation(responder, token, evaluated)
        });
        assert_eq!(reports.load(Ordering::Relaxed), 1);
        assert!(matches!(
            completion.completion,
            OwnedCompletionOutcome::Cancelled
        ));
        assert!(matches!(
            completion.delivery,
            OwnedDeliveryOutcome::Delivered
        ));
        assert_eq!(crate::test_callback::async_return_calls(), 1);
    }

    struct Pending {
        callbacks_at_drop: Arc<AtomicUsize>,
        panic_on_drop: bool,
    }
    impl Future for Pending {
        type Output = XllResult<f64>;
        fn poll(self: std::pin::Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
            Poll::Pending
        }
    }
    impl Drop for Pending {
        fn drop(&mut self) {
            self.callbacks_at_drop.store(
                crate::test_callback::async_return_calls(),
                Ordering::Relaxed,
            );
            assert!(
                !self.panic_on_drop,
                "injected forced future destructor panic"
            );
        }
    }

    #[test]
    fn miri_abandoned_async_evaluation_contains_value_destructor_panics() {
        struct Value<'a>(&'a AtomicUsize);

        impl crate::value::IntoExcel for Value<'_> {
            fn into_excel(self) -> XllResult<crate::value::ExcelCellOutput> {
                Ok(crate::value::ExcelCellOutput::Number(42.0))
            }
        }

        impl Drop for Value<'_> {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Relaxed);
                panic!("injected evaluation value destructor panic");
            }
        }

        struct Ready {
            drops: &'static AtomicUsize,
            future_panics_on_drop: bool,
        }

        impl Future for Ready {
            type Output = XllResult<Value<'static>>;

            fn poll(self: std::pin::Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
                Poll::Ready(Ok(Value(self.drops)))
            }
        }

        impl Drop for Ready {
            fn drop(&mut self) {
                assert!(
                    !self.future_panics_on_drop,
                    "injected ready future destructor panic"
                );
            }
        }

        static DROPS: AtomicUsize = AtomicUsize::new(0);
        let _runtime = crate::runtime::tests::TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _callbacks = crate::test_callback::lock();
        crate::test_callback::install();
        for (cancelled, future_panics_on_drop) in [(true, false), (false, true)] {
            crate::test_callback::reset();
            DROPS.store(0, Ordering::Relaxed);
            #[cfg(feature = "async-builtin")]
            crate::test_callback::set_async_rejected(future_panics_on_drop);
            let (source, token) = CancellationSource::new(CancellationGuarantee::BestEffort);
            if cancelled {
                source.cancel();
            }
            let mut raw = XLOPER12 {
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
            // SAFETY: the responder copies this live, initialized handle.
            let responder =
                unsafe { ExcelAsyncResponder::from_raw("DROP.VALUE", &mut raw).unwrap() };
            let mut task = std::pin::pin!(
                PendingDelivery::new(responder, token, NoObservation).task(Ready {
                    drops: &DROPS,
                    future_panics_on_drop
                })
            );
            assert!(
                task.as_mut()
                    .poll(&mut Context::from_waker(
                        futures_util::task::noop_waker_ref()
                    ))
                    .is_ready()
            );
            assert_eq!(DROPS.load(Ordering::Relaxed), 1);
            assert_eq!(crate::test_callback::async_return_calls(), 1);
        }
    }

    struct Observer {
        callbacks_at_drop: Arc<AtomicUsize>,
        observed: Arc<AtomicUsize>,
    }
    impl UdfLayerGuard for Observer {
        fn exit(self, outcome: &crate::execution::CallOutcome<'_>) {
            let cleaned_first = self.callbacks_at_drop.load(Ordering::Relaxed) == 0;
            let cancelled = matches!(outcome.completion, UdfCompletionOutcome::Cancelled);
            let delivery = match outcome.delivery {
                UdfDeliveryOutcome::Delivered => 1,
                UdfDeliveryOutcome::Failed { .. } => 2,
                _ => 0,
            };
            self.observed.store(
                if cleaned_first && cancelled {
                    delivery
                } else {
                    0
                },
                Ordering::Relaxed,
            );
        }
    }

    #[test]
    fn miri_unobserved_task_drop_preserves_cleanup_before_one_fallback() {
        let _runtime = crate::runtime::tests::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _callbacks = crate::test_callback::lock();
        crate::test_callback::install();
        for polled in [false, true] {
            for cancelled in [false, true] {
                for panic_on_drop in [false, true] {
                    crate::test_callback::reset();
                    let (source, token) =
                        CancellationSource::new(CancellationGuarantee::BestEffort);
                    let callbacks_at_drop = Arc::new(AtomicUsize::new(usize::MAX));
                    let mut raw = XLOPER12 {
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
                    // SAFETY: the live, initialized handle is copied by the responder.
                    let responder = unsafe {
                        ExcelAsyncResponder::from_raw("DROP.UNOBSERVED", &mut raw).unwrap()
                    };
                    let mut task = Box::pin(
                        PendingDelivery::new(responder, token, NoObservation).task(Pending {
                            callbacks_at_drop: Arc::clone(&callbacks_at_drop),
                            panic_on_drop,
                        }),
                    );
                    if polled {
                        assert!(
                            task.as_mut()
                                .poll(&mut Context::from_waker(
                                    futures_util::task::noop_waker_ref(),
                                ))
                                .is_pending()
                        );
                    }
                    if cancelled {
                        source.cancel();
                    }
                    drop(task);
                    assert_eq!(callbacks_at_drop.load(Ordering::Relaxed), 0);
                    assert_eq!(crate::test_callback::async_return_calls(), 1);
                }
            }
        }
    }

    #[test]
    fn forced_task_drop_disposes_user_future_before_delivery_and_reports_callback_outcome() {
        let _runtime = crate::runtime::tests::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _callbacks = crate::test_callback::lock();
        crate::test_callback::install();
        #[cfg(feature = "async-builtin")]
        let rejections = [false, true];
        #[cfg(not(feature = "async-builtin"))]
        let rejections = [false];
        for polled in [false, true] {
            for (rejected, panic_on_drop) in rejections
                .into_iter()
                .flat_map(|rejected| [false, true].map(|panic_on_drop| (rejected, panic_on_drop)))
            {
                crate::test_callback::reset();
                #[cfg(feature = "async-builtin")]
                crate::test_callback::set_async_rejected(rejected);
                let (source, token) = CancellationSource::new(CancellationGuarantee::BestEffort);
                let callbacks_at_drop = Arc::new(AtomicUsize::new(usize::MAX));
                let observed = Arc::new(AtomicUsize::new(0));
                let metadata = CallMetadata {
                    udf_id: "DROP.ORDER",
                    excel_name: "DROP.ORDER",
                    call_id: CallId::new(1),
                    calculation_id: CalculationId::new(1),
                    started_at: std::time::SystemTime::UNIX_EPOCH,
                    concurrent_calls: 1,
                };
                let observation = super::super::instrumentation::AsyncObservation::new(
                    &metadata,
                    CallTimer::start(),
                    Some(Observer {
                        callbacks_at_drop: Arc::clone(&callbacks_at_drop),
                        observed: Arc::clone(&observed),
                    }),
                    false,
                    token,
                );
                let mut raw = XLOPER12 {
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
                // SAFETY: the live, initialized handle is copied by the responder.
                let responder =
                    unsafe { ExcelAsyncResponder::from_raw("DROP.ORDER", &mut raw).unwrap() };
                let mut task = Box::pin(PendingDelivery::new(responder, token, observation).task(
                    Pending {
                        callbacks_at_drop: Arc::clone(&callbacks_at_drop),
                        panic_on_drop,
                    },
                ));
                if polled {
                    assert!(
                        task.as_mut()
                            .poll(&mut Context::from_waker(
                                futures_util::task::noop_waker_ref()
                            ))
                            .is_pending()
                    );
                }
                source.cancel();
                drop(task);
                assert_eq!(callbacks_at_drop.load(Ordering::Relaxed), 0);
                assert_eq!(
                    observed.load(Ordering::Relaxed),
                    if rejected { 2 } else { 1 }
                );
                assert_eq!(crate::test_callback::async_return_calls(), 1);
            }
        }
    }
}
