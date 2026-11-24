// module: async_udf/completion
use super::{NoObservation, PendingDelivery};

fn complete_twice(delivery: PendingDelivery<NoObservation>) {
    delivery.complete(Ok(Ok(42.0_f64)));
    delivery.complete(Ok(Ok(43.0_f64))); // error: E0382
}

fn fail_then_fallback(delivery: PendingDelivery<NoObservation>) {
    delivery.fail(crate::XllError::Closing);
    delivery.fallback(None); // error: E0382
}

fn fallback_then_fail(delivery: PendingDelivery<NoObservation>) {
    delivery.fallback(Some("panic".to_owned()));
    delivery.fail(crate::XllError::Closing); // error: E0382
}
