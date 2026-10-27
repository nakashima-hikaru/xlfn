// module: async_udf
use super::{executor::AsyncExecutor, runtime::AsyncRuntime};

fn bypass<E: AsyncExecutor>(runtime: &AsyncRuntime<E>) {
    let _ = &runtime.state; // error: E0616
}
