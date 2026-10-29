#[path = "../async_executor.rs"]
mod executor;

use xlfn::prelude::*;

struct State;

struct StaticHandleLeaseAddin;

#[excel_addin(name = "Static Handle Lease", id = "static-handle-lease", category = "Test")]
impl Addin for StaticHandleLeaseAddin {
    type AsyncExecutor = executor::Executor;
    type SharedState = State;
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(_: &OpenContext<Self>) -> OpenResult<Self> {
        Ok(Opened::new(State).with_async_executor(executor::Executor))
    }
}

#[derive(ExcelHandleObject)]
struct Dataset;

#[excel_function(name = "TEST.STATIC.HANDLE.LEASE")]
async fn static_handle_lease(dataset: HandleLease<'static, Dataset>) -> f64 {
    let _ = dataset;
    0.0
}

fn main() {}
