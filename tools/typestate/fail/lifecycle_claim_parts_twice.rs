// module: runtime
use crate::lifecycle::IssuedRemovalClaim;
use super::shutdown::FinalRemoval;

fn invalid<A: crate::Addin>(claim: IssuedRemovalClaim<'_, A, FinalRemoval>) {
    let _ = claim.into_issuer_parts();
    let _ = claim.into_issuer_parts(); // error: E0382
}
