// module: async_udf/runtime
use super::{AsyncExecutor, AwaitingTasks, ExecutorClose};

fn invalid<'a, E: AsyncExecutor>(closing: ExecutorClose<'a, E, AwaitingTasks<'a>>) {
    let _ = closing.wait();
    let _ = closing.wait(); // error: E0382
}
