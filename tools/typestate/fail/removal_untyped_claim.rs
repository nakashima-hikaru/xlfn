// module: runtime
use super::capabilities::ShutdownDeps;
use super::shutdown::{FinalRemoval, RemovalOwner};

fn invalid<'issuer, A: crate::Addin>(deps: ShutdownDeps<'issuer, A>, claim: crate::lifecycle::IssuedRemovalClaim<'issuer, A, FinalRemoval>) {
    let _: RemovalOwner<'_, A, FinalRemoval> = RemovalOwner::from_claim(deps, claim); // error: E0599
}
