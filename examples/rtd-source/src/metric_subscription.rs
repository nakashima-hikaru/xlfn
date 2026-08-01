use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::JoinHandle;

use xlfn::prelude::*;

pub(crate) struct MetricSubscription {
    pub(crate) cancelled: Arc<AtomicBool>,
    pub(crate) worker: Option<JoinHandle<()>>,
}

// SAFETY: request_cancel stops the sole producer, and disconnect_and_wait
// joins that producer before returning.
unsafe impl RtdSubscription for MetricSubscription {
    fn request_cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    fn disconnect_and_wait(mut self: Box<Self>) -> XllResult<()> {
        self.request_cancel();
        if let Some(worker) = self.worker.take() {
            worker.join().map_err(|_| XllError::Internal {
                diagnostic_id: 0x5254_4457_4f52_4b52,
            })?;
        }
        Ok(())
    }
}
