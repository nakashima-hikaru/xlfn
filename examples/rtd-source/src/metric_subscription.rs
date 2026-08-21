use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::JoinHandle;

use xlfn::prelude::*;
use xlfn::rtd::{RtdCancellation, RtdCancellationHandle, RtdSubscription};

pub(crate) struct MetricSubscription {
    pub(crate) cancelled: Arc<AtomicBool>,
    pub(crate) worker: Option<JoinHandle<()>>,
}

impl RtdSubscription for MetricSubscription {
    fn cancellation(&self) -> Arc<dyn RtdCancellation> {
        let cancelled = Arc::clone(&self.cancelled);
        Arc::new(RtdCancellationHandle::new(move || {
            cancelled.store(true, Ordering::Relaxed);
        }))
    }

    fn disconnect_and_wait(mut self: Box<Self>) -> XllResult<()> {
        self.cancelled.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            worker.join().map_err(|_| XllError::Panic)?;
        }
        Ok(())
    }
}
