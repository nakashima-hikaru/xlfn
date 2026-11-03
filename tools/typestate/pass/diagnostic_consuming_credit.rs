// module: diagnostics/worker
use super::{QueuedCredit, DeliveryCredit};
fn ordered(credit: QueuedCredit) -> DeliveryCredit { credit.dequeue() }
