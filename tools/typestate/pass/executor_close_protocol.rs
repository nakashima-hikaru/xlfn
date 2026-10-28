// module: async_udf/runtime
use super::{AsyncExecutor, ExecutorClose, PublicationsSealed};

fn close<E: AsyncExecutor>(
    closing: ExecutorClose<'_, E, PublicationsSealed>,
) -> crate::XllResult<()> {
    let _ = closing.drain_readers()?.wait().shutdown()?;
    Ok(())
}
