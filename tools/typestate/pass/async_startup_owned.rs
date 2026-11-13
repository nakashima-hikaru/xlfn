// module: async_udf/runtime
use super::{AsyncRuntime, startup::StartupClaim};
fn valid<E: crate::AsyncExecutor>(runtime: &AsyncRuntime<E>, executor: E) -> crate::XllResult<()> {
    StartupClaim::acquire(runtime, executor)?.publish(crate::addin_impl::AsyncTaskLimit::DEFAULT)
}
