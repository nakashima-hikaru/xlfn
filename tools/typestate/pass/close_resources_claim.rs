// module: lifecycle/state
use super::{AvailableClose, ClaimedClose};
use crate::generation::RemovalAttemptId;
use crate::module_runtime::ModuleClosing;

fn claim<A: crate::Addin>(
    available: AvailableClose<A>,
    attempt: RemovalAttemptId,
) -> (ClaimedClose<A>, ModuleClosing) {
    available.claim(attempt)
}
