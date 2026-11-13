// module: async_udf
use super::registry::TaskRegistry;

fn invalid(registry: &TaskRegistry) -> crate::XllResult<()> {
    let prepared = registry.prepare_start(crate::addin_impl::AsyncTaskLimit::DEFAULT)?;
    prepared.publish();
    prepared.publish(); // error: E0382
    Ok(())
}
