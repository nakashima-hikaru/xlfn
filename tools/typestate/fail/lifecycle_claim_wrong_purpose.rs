// module: runtime
use crate::lifecycle::IssuedRemovalClaim;
use super::shutdown::{FinalRemoval, OpenRollback};

fn invalid<A: crate::Addin>(claim: IssuedRemovalClaim<'_, A, FinalRemoval>) {
    let _: IssuedRemovalClaim<'_, A, OpenRollback> = claim; // error: E0308
}
