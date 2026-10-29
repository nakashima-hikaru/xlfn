#[path = "../async_executor.rs"]
mod executor;

use xlfn::prelude::*;
use xlfn::value::XlArrayRef;

type BorrowedArray<'call> = XlArrayRef<'call>;

struct BorrowedAsyncAliasAddin;

#[excel_addin(name = "Borrowed Async Alias", id = "borrowed-async-alias", category = "Test")]
impl Addin for BorrowedAsyncAliasAddin {
    type AsyncExecutor = executor::Executor;
    type SharedState = ();
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(_: &OpenContext<Self>) -> OpenResult<Self> {
        Ok(Opened::new(()).with_async_executor(executor::Executor))
    }
}

#[excel_function(name = "FAIL.BORROWED.ASYNC.ALIAS")]
async fn bad(values: BorrowedArray<'_>) -> f64 {
    values.len() as f64
}

fn main() {}
