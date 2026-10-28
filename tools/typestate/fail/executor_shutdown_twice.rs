// module: async_udf/runtime
use super::{AsyncExecutor, ExecutorClose, ExecutorTasksDrained};

fn invalid<'a, E: AsyncExecutor>(closing: ExecutorClose<'a, E, ExecutorTasksDrained<'a>>) {
    let _ = closing.shutdown();
    let _ = closing.shutdown(); // error: E0382
}
