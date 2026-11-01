// module: async_udf/runtime
use super::startup::StartupClaim;
fn invalid<E: crate::AsyncExecutor>(claim: StartupClaim<'_, E>) {
    let _ = claim.owner; // error: E0616
}
