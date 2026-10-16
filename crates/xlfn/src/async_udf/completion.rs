use super::transport::ExcelAsyncResponder;
use crate::call_return::{ExcelReturn, ReturnContext};
use crate::cancellation::CancellationToken;
use crate::error::ExcelCallbackStatus;
use crate::panic_boundary::catch_no_unwind;
use crate::return_abi::AsyncReturnValue;
use crate::{XllError, XllResult};
use futures_util::Future;
use std::panic::AssertUnwindSafe;
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

/// Owns delivery and observation together so forced destruction reports the
/// result of the fallback callback rather than guessing that it was unobserved.
pub(crate) struct AsyncDelivery<G: crate::execution::UdfLayerGuard = ()> {
    responder: ExcelAsyncResponder,
    token: CancellationToken,
    observation: Option<super::instrumentation::AsyncObservation<G>>,
    completed: bool,
}

impl<G: crate::execution::UdfLayerGuard> AsyncDelivery<G> {
    pub(crate) fn new(
        responder: ExcelAsyncResponder,
        token: CancellationToken,
        observation: Option<super::instrumentation::AsyncObservation<G>>,
    ) -> Self {
        Self {
            responder,
            token,
            observation,
            completed: false,
        }
    }

    pub(crate) fn task<Fut, T>(self, future: Fut) -> AsyncUdfTask<Fut, G>
    where
        Fut: Future<Output = XllResult<T>> + Send,
        T: ExcelReturn + Send + 'static,
    {
        AsyncUdfTask {
            future: super::future::NoUnwindFuture::new(self.responder.udf_id, future),
            delivery: self,
        }
    }

    fn finish(&mut self, completion: AsyncCompletion) {
        self.completed = true;
        super::boundary::report_completion(self.responder.udf_id, &completion);
        if let Some(observation) = self.observation.take() {
            observation.finish(&completion);
        }
    }

    pub(crate) fn fallback(&mut self, panicked: bool) {
        if self.completed {
            return;
        }
        let cancelled = self.token.is_cancelled();
        if cancelled && panicked {
            crate::diagnostics::report_no_unwind(self.responder.udf_id, &XllError::Panic);
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
        // SAFETY: the responder owns its validated, copied Excel handle.
        let delivery = match unsafe { self.responder.deliver_error(&error) } {
            Ok(()) => OwnedDeliveryOutcome::Delivered,
            Err(error) => OwnedDeliveryOutcome::Failed(error),
        };
        self.finish(AsyncCompletion {
            completion,
            delivery,
        });
    }
}

impl<G: crate::execution::UdfLayerGuard> Drop for AsyncDelivery<G> {
    fn drop(&mut self) {
        self.fallback(false);
    }
}

/// Explicit fields enforce cleanup order even when the task is never polled.
/// User destruction is contained, and precedes Excel delivery and layer exit.
pub(crate) struct AsyncUdfTask<Fut, G: crate::execution::UdfLayerGuard> {
    future: super::future::NoUnwindFuture<Fut>,
    delivery: AsyncDelivery<G>,
}

impl<Fut, G: crate::execution::UdfLayerGuard> Drop for AsyncUdfTask<Fut, G> {
    fn drop(&mut self) {
        // SAFETY: the field is destroyed in place and never moved after polling.
        let panicked = unsafe { std::pin::Pin::new_unchecked(&mut self.future) }
            .finish()
            .is_err();
        self.delivery.fallback(panicked);
    }
}

impl<Fut, T, G> Future for AsyncUdfTask<Fut, G>
where
    Fut: Future<Output = XllResult<T>> + Send,
    T: ExcelReturn + Send + 'static,
    G: crate::execution::UdfLayerGuard,
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
        let evaluated = match future.finish() {
            Ok(()) => evaluated,
            Err(panic) => Err(panic),
        };
        #[cfg(test)]
        if let Some(hook) = *super::boundary::AFTER_ASYNC_EVALUATION_HOOK.lock() {
            hook();
        }
        let completion =
            complete_evaluation(&mut this.delivery.responder, this.delivery.token, evaluated);
        this.delivery.finish(completion);
        std::task::Poll::Ready(())
    }
}

fn complete_evaluation<T: ExcelReturn + Send + 'static>(
    responder: &mut ExcelAsyncResponder,
    token: CancellationToken,
    evaluated: Result<XllResult<T>, crate::panic_boundary::CaughtPanic>,
) -> AsyncCompletion {
    if !token.try_start_delivery() {
        let cancel_error = XllError::ExcelValue(crate::ExcelError::NotAvailable);
        // SAFETY: the responder owns the copied async handle.
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

/// Delivers an error for a raw async handle before ownership can be wrapped in
/// an [`ExcelAsyncResponder`].
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
mod tests {
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
                let mut task = Box::pin(
                    AsyncDelivery::new(responder, token, Some(observation)).task(Pending {
                        callbacks_at_drop: Arc::clone(&callbacks_at_drop),
                        panic_on_drop,
                    }),
                );
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
