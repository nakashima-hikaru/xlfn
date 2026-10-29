// module: runtime
use super::orchestration::RemovalPurposeClaim;
use super::shutdown::OpenRollback;

fn invalid(claim: crate::lifecycle::RemovalClaim) {
    let _ = RemovalPurposeClaim::<OpenRollback>::issue(claim); // error: E0624
}
