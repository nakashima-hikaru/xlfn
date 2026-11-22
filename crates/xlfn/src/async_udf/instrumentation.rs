use super::completion::{AsyncCompletion, OwnedCompletionOutcome, OwnedDeliveryOutcome};
use crate::XllError;
use crate::cancellation::CancellationToken;
use crate::execution::{
    CallMetadata, CallOutcome, CallTimer, UdfCompletionOutcome, UdfDeliveryOutcome, UdfLayerGuard,
    UdfTraceMetadata, safe_exit, trace,
};
use xlfn_kernel::rollback::{Rollback, RollbackGuard};

/// Completion observation is selected when an invocation chooses its path.
/// Uninstrumented tasks carry a zero-sized policy instead of optional metadata.
pub(crate) trait CompletionObserver {
    fn finish(self, completion: &AsyncCompletion);
}

pub(crate) struct NoObservation;

impl CompletionObserver for NoObservation {
    fn finish(self, _: &AsyncCompletion) {}
}

/// Observation state for one async UDF invocation uniquely owned by the running task.
///
/// The observation plane owns timing, layer guards, trace metadata, and the
/// cancellation token needed to classify a forced task drop. It has no Excel
/// handle or executor ownership.
pub(crate) struct AsyncObservation<G: UdfLayerGuard> {
    active: RollbackGuard<ActiveUdfInstrumentation<G>>,
}

struct ActiveUdfInstrumentation<G: UdfLayerGuard> {
    udf_id: &'static str,
    excel_name: &'static str,
    call_id: crate::execution::CallId,
    calculation_id: crate::execution::CalculationId,
    concurrent_calls: usize,
    timer: CallTimer,
    layers: Option<G>,
    trace_enabled: bool,
    cancellation: CancellationToken,
}

impl<G: UdfLayerGuard> AsyncObservation<G> {
    pub(crate) fn new(
        metadata: &CallMetadata,
        timer: CallTimer,
        layers: Option<G>,
        trace_enabled: bool,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            active: RollbackGuard::new(ActiveUdfInstrumentation {
                udf_id: metadata.udf_id,
                excel_name: metadata.excel_name,
                call_id: metadata.call_id,
                calculation_id: metadata.calculation_id,
                concurrent_calls: metadata.concurrent_calls,
                timer,
                layers,
                trace_enabled,
                cancellation,
            }),
        }
    }
}

impl<G: UdfLayerGuard> CompletionObserver for AsyncObservation<G> {
    fn finish(self, completion: &AsyncCompletion) {
        finish_active(self.active.complete(), completion);
    }
}

impl<G: UdfLayerGuard> Rollback for ActiveUdfInstrumentation<G> {
    fn rollback(self) {
        let completion = if self.cancellation.is_cancelled() {
            OwnedCompletionOutcome::Cancelled
        } else {
            OwnedCompletionOutcome::Error(XllError::Internal {
                diagnostic_id: crate::diagnostics::id::DiagnosticId::ASYNC_COMPLETION,
            })
        };
        finish_active(
            self,
            &AsyncCompletion {
                completion,
                delivery: OwnedDeliveryOutcome::Unobserved,
            },
        );
    }
}

fn finish_active<G: UdfLayerGuard>(
    active: ActiveUdfInstrumentation<G>,
    completion: &AsyncCompletion,
) {
    let ActiveUdfInstrumentation {
        udf_id,
        excel_name,
        call_id,
        calculation_id,
        concurrent_calls,
        timer,
        layers,
        trace_enabled,
        cancellation: _,
    } = active;

    let outcome = call_outcome(completion, timer.elapsed());

    if let Some(layers) = layers {
        safe_exit(layers, &outcome);
    }
    if trace_enabled {
        let metadata = UdfTraceMetadata {
            udf_id,
            excel_name,
            call_id,
            calculation_id,
            concurrent_calls,
        };
        trace(&metadata, &outcome);
    }
}

impl crate::execution::LayerRejection for (AsyncCompletion, std::time::Duration) {
    fn outcome(&self) -> CallOutcome<'_> {
        call_outcome(&self.0, self.1)
    }
}

/// A layer can reject before ownership of a complete guard tuple exists.
/// That invocation still has an observed response and trace outcome.
pub(crate) fn trace_rejected_launch(
    metadata: &CallMetadata,
    duration: std::time::Duration,
    completion: &AsyncCompletion,
) {
    let outcome = call_outcome(completion, duration);
    let metadata = UdfTraceMetadata {
        udf_id: metadata.udf_id,
        excel_name: metadata.excel_name,
        call_id: metadata.call_id,
        calculation_id: metadata.calculation_id,
        concurrent_calls: metadata.concurrent_calls,
    };
    trace(&metadata, &outcome);
}

fn call_outcome(completion: &AsyncCompletion, duration: std::time::Duration) -> CallOutcome<'_> {
    let completion_outcome = match &completion.completion {
        OwnedCompletionOutcome::Success => UdfCompletionOutcome::Success,
        OwnedCompletionOutcome::Error(error) => UdfCompletionOutcome::from_error(error),
        OwnedCompletionOutcome::Cancelled => UdfCompletionOutcome::Cancelled,
    };
    let delivery_outcome = match &completion.delivery {
        OwnedDeliveryOutcome::Delivered => UdfDeliveryOutcome::Delivered,
        OwnedDeliveryOutcome::Failed(error) => UdfDeliveryOutcome::Failed { error },
        OwnedDeliveryOutcome::Unobserved => UdfDeliveryOutcome::Unobserved,
    };
    CallOutcome {
        completion: completion_outcome,
        delivery: delivery_outcome,
        duration,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::async_udf::completion::PendingDelivery;
    use crate::async_udf::transport::ExcelAsyncResponder;
    use crate::cancellation::{CancellationGuarantee, CancellationSource};
    use crate::execution::{CalculationId, CallId, UdfErrorKind};
    use std::sync::mpsc::{Receiver, Sender};
    use std::time::SystemTime;
    use xlfn_sys::{
        XLOPER12, XLOPER12BigData, XLOPER12BigDataHandle, XLOPER12Value, XLTYPE_BIG_DATA,
    };

    #[derive(Debug, Eq, PartialEq)]
    enum CompletionSnapshot {
        Success,
        Error(UdfErrorKind, Option<i32>),
        Cancelled,
    }

    #[derive(Debug, Eq, PartialEq)]
    enum DeliverySnapshot {
        NotApplicable,
        Delivered,
        Failed,
        Unobserved,
    }

    struct RecordingGuard(Sender<(CompletionSnapshot, DeliverySnapshot)>);

    impl UdfLayerGuard for RecordingGuard {
        fn exit(self, outcome: &CallOutcome<'_>) {
            let completion = match outcome.completion {
                UdfCompletionOutcome::Success => CompletionSnapshot::Success,
                UdfCompletionOutcome::Error {
                    kind, vendor_code, ..
                } => CompletionSnapshot::Error(kind, vendor_code),
                UdfCompletionOutcome::Cancelled => CompletionSnapshot::Cancelled,
            };
            let delivery = match outcome.delivery {
                UdfDeliveryOutcome::NotApplicable => DeliverySnapshot::NotApplicable,
                UdfDeliveryOutcome::Delivered => DeliverySnapshot::Delivered,
                UdfDeliveryOutcome::Failed { .. } => DeliverySnapshot::Failed,
                UdfDeliveryOutcome::Unobserved => DeliverySnapshot::Unobserved,
            };
            self.0.send((completion, delivery)).unwrap();
        }
    }

    fn metadata() -> CallMetadata {
        CallMetadata {
            udf_id: "test_async_observation",
            excel_name: "TEST.ASYNC.OBSERVATION",
            call_id: CallId::new(1),
            calculation_id: CalculationId::new(1),
            started_at: SystemTime::UNIX_EPOCH,
            concurrent_calls: 1,
        }
    }

    fn observation(
        sender: Sender<(CompletionSnapshot, DeliverySnapshot)>,
        token: CancellationToken,
    ) -> AsyncObservation<RecordingGuard> {
        AsyncObservation::new(
            &metadata(),
            CallTimer::start(),
            Some(RecordingGuard(sender)),
            false,
            token,
        )
    }

    fn receive(
        receiver: Receiver<(CompletionSnapshot, DeliverySnapshot)>,
    ) -> (CompletionSnapshot, DeliverySnapshot) {
        receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap()
    }

    #[test]
    fn cancelled_observation_drop_is_unobserved_cancellation() {
        let (source, token) = CancellationSource::new(CancellationGuarantee::BestEffort);
        let (sender, receiver) = std::sync::mpsc::channel();
        let observation = observation(sender, token);
        source.cancel();
        drop(observation);

        assert_eq!(
            receive(receiver),
            (CompletionSnapshot::Cancelled, DeliverySnapshot::Unobserved,)
        );
    }

    #[test]
    fn miri_pending_delivery_drop_observes_one_fallback() {
        let _callbacks = crate::test_callback::lock();
        crate::test_callback::install();
        crate::test_callback::reset();
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
        // SAFETY: the live, initialized token is copied by the responder.
        let responder =
            unsafe { ExcelAsyncResponder::from_raw(metadata().udf_id, &mut raw) }.unwrap();
        let (source, token) = CancellationSource::new(CancellationGuarantee::BestEffort);
        let (sender, receiver) = std::sync::mpsc::channel();
        let delivery = PendingDelivery::new(responder, token, observation(sender, token));
        source.cancel();
        drop(delivery);

        assert_eq!(crate::test_callback::async_return_calls(), 1);
        assert_eq!(
            receiver.try_recv().unwrap(),
            (CompletionSnapshot::Cancelled, DeliverySnapshot::Delivered),
        );
        assert_eq!(
            receiver.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Disconnected)
        );
    }

    #[test]
    fn unexpected_observation_drop_is_internal_and_unobserved() {
        let (_source, token) = CancellationSource::new(CancellationGuarantee::BestEffort);
        let (sender, receiver) = std::sync::mpsc::channel();
        let observation = observation(sender, token);
        drop(observation);

        assert_eq!(
            receive(receiver),
            (
                CompletionSnapshot::Error(UdfErrorKind::Internal, None),
                DeliverySnapshot::Unobserved,
            )
        );
    }

    #[test]
    fn completion_and_delivery_failures_remain_independent() {
        let (_source, token) = CancellationSource::new(CancellationGuarantee::BestEffort);
        let (sender, receiver) = std::sync::mpsc::channel();
        let observation = observation(sender, token);
        let computation_error = XllError::Native {
            code: 73,
            message: "computation".to_owned(),
        };
        let delivery_error = XllError::Panic;
        let completion = AsyncCompletion {
            completion: OwnedCompletionOutcome::Error(computation_error),
            delivery: OwnedDeliveryOutcome::Failed(delivery_error),
        };
        observation.finish(&completion);

        assert_eq!(
            receiver.recv().unwrap(),
            (
                CompletionSnapshot::Error(UdfErrorKind::Vendor, Some(73)),
                DeliverySnapshot::Failed,
            )
        );
        assert!(matches!(
            receiver.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Disconnected)
        ));
    }
}
