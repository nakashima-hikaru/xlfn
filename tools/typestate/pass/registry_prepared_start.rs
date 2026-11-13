// module: async_udf
use super::registry::TaskRegistry;

fn valid(registry: &TaskRegistry) -> crate::XllResult<()> {
    registry.prepare_start(crate::addin_impl::AsyncTaskLimit::DEFAULT)?.publish();
    Ok(())
}
