#[path = "../async_executor.rs"]
mod executor;

use xlfn::prelude::*;

struct TestAddin;

#[excel_addin(name = "Context Async", id = "context-async", category = "Test")]
impl Addin for TestAddin {
    type AsyncExecutor = executor::Executor;
    type SharedState = ();
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(_: &OpenContext<Self>) -> OpenResult<Self> {
        Ok(Opened::new(()).with_async_executor(executor::Executor))
    }
}

type WorkerContext<'call> = AsyncContext<'call, TestAddin>;

#[excel_function]
async fn bad(context: WorkerContext<'_>) -> f64 {
    let _ = context;
    0.0
}

fn main() {}
