use xlfn::prelude::*;

#[excel_addin(name = "Context Async", id = "context-async", category = "Test")]
struct TestAddin;

impl Addin for TestAddin {
    type SharedState = ();
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();
    #[cfg(feature = "async")]
    type AsyncExecutor = xlfn::NoAsyncExecutor;

    fn open(_: &OpenContext) -> OpenResult<Self> {
        Ok(Opened::new(()))
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
