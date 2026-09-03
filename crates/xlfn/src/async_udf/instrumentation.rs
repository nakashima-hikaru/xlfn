use super::completion::{AsyncCompletion, OwnedCompletionOutcome, OwnedDeliveryOutcome};
use crate::XllError;
use crate::cancellation::CancellationToken;
use crate::execution::{
    CallMetadata, CallOutcome, CallTimer, UdfCompletionOutcome, UdfDeliveryOutcome, UdfLayerGuard,
    UdfTraceMetadata, safe_exit, trace,
};

/// Observation state for one async UDF invocation uniquely owned by the running task.
///
/// The observation plane owns timing, layer guards, trace metadata, and the
/// cancellation token needed to classify a forced task drop. It has no Excel
/// handle or executor ownership.
pub(crate) struct AsyncObservation<G: UdfLayerGuard> {
    active: Option<ActiveUdfInstrumentation<G>>,
}

pub(crate) struct ActiveUdfInstrumentation<G: UdfLayerGuard> {
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
            active: Some(ActiveUdfInstrumentation {
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

    pub(crate) fn finish(mut self, completion: &AsyncCompletion) {
        if let Some(active) = self.active.take() {
            finish_active(active, completion);
        }
    }
}

impl<G: UdfLayerGuard> Drop for AsyncObservation<G> {
    fn drop(&mut self) {
        if let Some(active) = self.active.take() {
            let completion = if active.cancellation.is_cancelled() {
                AsyncCompletion {
                    completion: OwnedCompletionOutcome::Cancelled,
                    delivery: OwnedDeliveryOutcome::Unobserved,
                }
            } else {
                AsyncCompletion {
                    completion: OwnedCompletionOutcome::Error(XllError::Internal {
                        diagnostic_id: crate::diagnostics::id::DiagnosticId::ASYNC_COMPLETION,
                    }),
                    delivery: OwnedDeliveryOutcome::Unobserved,
                }
            };
            finish_active(active, &completion);
        }
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
    let outcome = CallOutcome {
        completion: completion_outcome,
        delivery: delivery_outcome,
        duration: timer.elapsed(),
    };

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cancellation::{CancellationGuarantee, CancellationSource};
    use crate::execution::{CalculationId, CallId, UdfErrorKind};
    use std::sync::mpsc::{Receiver, Sender};
    use std::time::SystemTime;

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
            receive(receiver),
            (
                CompletionSnapshot::Error(UdfErrorKind::Vendor, Some(73)),
                DeliverySnapshot::Failed,
            )
        );
    }
}
