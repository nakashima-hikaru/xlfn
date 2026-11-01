// module: async_udf/runtime
use super::AsyncExecutor;
use super::closing::{AwaitingTasks, ExecutorClose};

fn invalid<'a, E: AsyncExecutor>(closing: ExecutorClose<'a, E, AwaitingTasks<'a>>) {
    let _ = closing.shutdown(); // error: E0599
}
