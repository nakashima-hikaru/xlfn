// module: runtime
use crate::lifecycle::{IssuedRemovalClaim, LifecycleCoordinator};
use super::shutdown::FinalRemoval;

fn invalid<'issuer, A: crate::Addin>(mut claim: IssuedRemovalClaim<'issuer, A, FinalRemoval>, other: &'issuer LifecycleCoordinator<A>) {
    claim.issuer = other; // error: E0616
}
