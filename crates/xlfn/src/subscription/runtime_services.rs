//! Shared capabilities for subscription operations that outlive the control
//! plane runtime.
//!
//! `PublishCore` is a data-plane object and must not retain a back-reference
//! to `SubscriptionRuntime`.  It only needs to publish observations and keep
//! cleanup failures until the owning runtime can report them.  Keeping those
//! two capabilities here gives data-plane callbacks a stable owner without
//! reopening access to the subscription catalog or server registry.

use crate::shutdown_trace::{ObservationSink, ShutdownEvent, ShutdownTraceHandle};
use crate::{XllError, XllResult};
use parking_lot::Mutex;

pub(crate) struct RuntimeServices {
    cleanup_failure: Mutex<Option<XllError>>,
    observer: ObservationSink,
}

impl RuntimeServices {
    pub(crate) const fn new() -> Self {
        Self {
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
