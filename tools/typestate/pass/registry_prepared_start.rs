// module: async_udf
use super::registry::TaskRegistry;

fn valid(registry: &TaskRegistry) -> crate::XllResult<()> {
    registry.prepare_start(crate::AsyncTaskLimit::DEFAULT)?.publish();
    Ok(())
}
