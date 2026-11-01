// module: runtime
use super::capabilities::ShutdownDeps;
use super::shutdown::{OpenRollback, RemovalOwner};

fn invalid<'issuer, A: crate::Addin>(deps: ShutdownDeps<'issuer, A>, claim: crate::lifecycle::IssuedRemovalClaim<'issuer, A, OpenRollback>) {
    let _ = RemovalOwner::<A, OpenRollback>::new(deps, claim); // error: E0624
}
