#[path = "../async_executor.rs"]
mod executor;

use xlfn::prelude::*;
use xlfn::value::XlArrayRef;

struct State;

struct BorrowedAsyncAddin;

#[excel_addin(name = "Borrowed Async", id = "borrowed-async", category = "Test")]
impl Addin for BorrowedAsyncAddin {
    type AsyncExecutor = executor::Executor;
    type SharedState = State;
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(_: &OpenContext<Self>) -> OpenResult<Self> {
        Ok(Opened::new(State).with_async_executor(executor::Executor))
    }
}

#[excel_function(name = "FAIL.BORROWED.ASYNC")]
async fn bad(values: XlArrayRef<'_>) -> f64 {
    values.len() as f64
}

fn main() {}
