// module: async_udf/completion
use super::{NoObservation, PendingDelivery};

fn complete(delivery: PendingDelivery<NoObservation>) {
    delivery.complete(Ok(Ok(42.0_f64)));
}

fn fail(delivery: PendingDelivery<NoObservation>) {
    delivery.fail(crate::XllError::Closing);
}

fn fallback(delivery: PendingDelivery<NoObservation>) {
    delivery.fallback(false);
}
