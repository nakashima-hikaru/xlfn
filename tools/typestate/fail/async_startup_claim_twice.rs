// module: async_udf/runtime
use super::startup::StartupClaim;
fn invalid<E: crate::AsyncExecutor>(claim: StartupClaim<'_, E>) {
    let _ = claim.publish(crate::addin_impl::AsyncTaskLimit::DEFAULT);
    let _ = claim.publish(crate::addin_impl::AsyncTaskLimit::DEFAULT); // error: E0382
}
