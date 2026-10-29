// module: runtime
use super::capabilities::ShutdownDeps;
use super::shutdown::{FinalRemoval, RemovalOwner};

fn invalid<A: crate::Addin>(deps: ShutdownDeps<'_, A>, claim: crate::lifecycle::RemovalClaim) {
    let _: RemovalOwner<'_, A, FinalRemoval> = RemovalOwner::from_claim(deps, claim); // error: E0599
}
