// module: runtime
use crate::lifecycle::{IssuedRemovalClaim, LifecycleCoordinator};
use super::shutdown::FinalRemoval;

fn invalid<A: crate::Addin>(claim: IssuedRemovalClaim<'_, A, FinalRemoval>, coordinator: &LifecycleCoordinator<A>) {
    let _ = claim.into_parts(coordinator);
    let _ = claim.into_parts(coordinator); // error: E0382
}
