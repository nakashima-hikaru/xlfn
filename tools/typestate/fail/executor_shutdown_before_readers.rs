// module: async_udf/runtime
use super::{AsyncExecutor, ExecutorClose, PublicationsSealed};

fn invalid<E: AsyncExecutor>(closing: ExecutorClose<'_, E, PublicationsSealed>) {
    let _ = closing.shutdown(); // error: E0599
}
