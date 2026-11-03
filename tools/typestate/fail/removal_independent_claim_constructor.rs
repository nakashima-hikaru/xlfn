// module: runtime
use super::capabilities::ShutdownDeps;
use super::shutdown::{FinalRemoval, RemovalOwner};
use crate::lifecycle::IssuedRemovalClaim;
fn invalid<'a, A: crate::Addin>(deps: ShutdownDeps<'a, A>, claim: IssuedRemovalClaim<'a, A, FinalRemoval>) {
    let _ = RemovalOwner::<A, FinalRemoval>::new(deps, claim); // error: E0599
}
