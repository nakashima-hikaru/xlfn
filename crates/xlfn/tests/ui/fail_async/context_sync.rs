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

#[excel_function]
async fn main_context(context: MainThreadContext<'_, TestAddin>) -> f64 {
    let _ = context;
    0.0
}

#[excel_function]
async fn thread_context(context: ThreadSafeContext<'_, TestAddin>) -> f64 {
    let _ = context;
    0.0
}

#[excel_function]
async fn macro_context(context: MacroSheetContext<'_, TestAddin>) -> f64 {
    let _ = context;
    0.0
}

fn main() {}
