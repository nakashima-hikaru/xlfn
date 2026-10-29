// module: runtime
use super::capabilities::ShutdownDeps;
use super::shutdown::{OpenRollback, RemovalOwner};

fn invalid<A: crate::Addin>(deps: ShutdownDeps<'_, A>, claim: crate::lifecycle::RemovalClaim) {
    let _ = RemovalOwner::<A, OpenRollback>::new(deps, claim); // error: E0624
}
