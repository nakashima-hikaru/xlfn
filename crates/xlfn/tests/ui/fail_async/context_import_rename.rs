use xlfn::prelude::*;

#[excel_addin(name = "Context Async", id = "context-async", category = "Test")]
struct TestAddin;

impl Addin for TestAddin {
    type RtdSubscription = ();
    type RtdSources = xlfn::DynamicRtdSources<Self::RtdSubscription>;
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

use xlfn::AsyncContext as WorkerContext;

#[excel_function]
async fn bad(context: WorkerContext<'_, TestAddin>) -> f64 {
    let _ = context;
    0.0
}

fn main() {}
