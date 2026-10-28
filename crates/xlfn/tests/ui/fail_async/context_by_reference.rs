use xlfn::prelude::*;

struct TestAddin;

#[excel_addin(name = "Context Async", id = "context-async", category = "Test")]
impl Addin for TestAddin {
    type SharedState = ();
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();
    #[cfg(feature = "async")]
    type AsyncExecutor = xlfn::NoAsyncExecutor;

    fn open(_: &OpenContext<Self>) -> OpenResult<Self> {
        Ok(Opened::new(()))
    }
}

#[excel_function]
async fn bad(context: &AsyncContext<'_, TestAddin>) -> f64 {
    let _ = context;
    0.0
}

fn main() {}
