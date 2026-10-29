// module: lifecycle/state
use super::{AvailableClose, ClaimedClose, CloseResources, UnavailableClose};
use crate::generation::RemovalAttemptId;

fn claimed<A: crate::Addin>(resources: ClaimedClose<A>, attempt: RemovalAttemptId) {
    let _ = resources.claim(attempt); // error: E0599
}

fn unavailable<A: crate::Addin>(resources: UnavailableClose<A>, attempt: RemovalAttemptId) {
    let _ = resources.claim(attempt); // error: E0599
}

fn unchecked<A: crate::Addin>(resources: CloseResources<A>, attempt: RemovalAttemptId) {
    let _ = resources.claim(attempt); // error: E0599
}

fn repeated<A: crate::Addin>(resources: AvailableClose<A>, attempt: RemovalAttemptId) {
    let _ = resources.claim(attempt);
    let _ = resources.claim(attempt); // error: E0382
}
