#[path = "../async_executor.rs"]
mod executor;

use xlfn::prelude::*;

struct State;

struct ReturnedHandleLeaseAddin;

#[excel_addin(name = "Returned Handle Lease", id = "returned-handle-lease", category = "Test")]
impl Addin for ReturnedHandleLeaseAddin {
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

#[excel_function(name = "TEST.RETURNED.HANDLE.LEASE")]
async fn returned_handle_lease(dataset: HandleLease<'_, Dataset>) -> HandleLease<'_, Dataset> {
    dataset
}

fn main() {}
