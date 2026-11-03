// module: subscription
use crate::rtd::{RtdLatest, RtdPendingBatch, RtdPendingValue, RtdSender};

fn retry<T>(fifo: &RtdSender<T>, latest: &RtdSender<T, RtdLatest>, value: RtdPendingValue<T>) {
    if let Err(error) = fifo.try_send_pending(value) {
        let _ = latest.try_send_pending(error.into_pending().unwrap());
    }
}

fn retry_batch<T>(
    fifo: &RtdSender<T>,
    latest: &RtdSender<T, RtdLatest>,
    batch: RtdPendingBatch<T>,
) {
    if let Err(error) = latest.try_send_pending_batch(batch) {
        let _ = fifo.try_send_pending_batch(error.into_pending().unwrap());
    }
}
