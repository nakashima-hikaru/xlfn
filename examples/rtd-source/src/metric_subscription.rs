use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::JoinHandle;

use xlfn::prelude::*;
use xlfn::rtd::RtdSubscription;

pub(crate) struct MetricSubscription {
    pub(crate) cancelled: Arc<AtomicBool>,
    pub(crate) worker: Option<JoinHandle<()>>,
}

// SAFETY: `disconnect_and_wait` signals the cancellation flag and joins the
// worker thread, ensuring no sink usage after return.
unsafe impl RtdSubscription for MetricSubscription {
    fn request_cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    fn disconnect_and_wait(mut self: Box<Self>) -> XllResult<()> {
        self.cancelled.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            worker.join().map_err(|_| XllError::Panic)?;
        }
        Ok(())
    }
}
