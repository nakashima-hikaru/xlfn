//! Shared capabilities kept alive while the subscription control plane shuts down.
//!
//! `PublishCore` is a data-plane object and must not retain a back-reference
//! to `SubscriptionRuntime`. Its admission gate, quotas, observations, and
//! cleanup failures live in this separate allocation, including while final
//! runtime Drop exclusively borrows the control plane and joins producers.

use crate::shutdown_trace::{ObservationSink, ShutdownEvent, ShutdownTraceHandle};
use crate::{XllError, XllResult};
use parking_lot::Mutex;
use xlfn_kernel::operation_gate::OperationGate;
use xlfn_kernel::quota::Quota;

pub(crate) struct RuntimeServices {
    pub(crate) runtime_gate: OperationGate,
    pub(crate) active_quota: Quota,
    pub(crate) queued_update_quota: Quota,
    cleanup_failure: Mutex<Option<XllError>>,
    observer: ObservationSink,
}

impl RuntimeServices {
    pub(crate) const fn new(limits: super::topic::RtdLimits) -> Self {
        Self {
            runtime_gate: OperationGate::new(),
            active_quota: Quota::new(limits.max_active.get()),
            queued_update_quota: Quota::new(limits.max_queued_updates.get()),
            cleanup_failure: Mutex::new(None),
            observer: ObservationSink::new(),
        }
    }

    pub(crate) fn set_trace_sink(&self, trace: ShutdownTraceHandle) {
        self.observer.set_trace_sink(trace);
    }

    #[inline]
    pub(crate) fn record(&self, event: ShutdownEvent) {
        self.observer.record(event);
    }

    pub(crate) fn record_cleanup_result(&self, result: XllResult<()>) {
        if let Err(error) = result {
            let mut failure = self.cleanup_failure.lock();
            if failure.is_none() {
                *failure = Some(error);
            }
        }
    }

    pub(crate) fn cleanup_result(&self) -> XllResult<()> {
        self.cleanup_failure
            .lock()
            .as_ref()
            .map_or(Ok(()), |error| Err(error.clone()))
    }
}
