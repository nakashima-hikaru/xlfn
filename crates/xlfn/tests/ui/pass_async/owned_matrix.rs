#[path = "../async_executor.rs"]
mod executor;

use xlfn::prelude::*;

struct State;

struct OwnedMatrixAsyncAddin;

#[excel_addin(
    name = "Owned Matrix Async",
    id = "owned-matrix-async",
    category = "Test"
)]
impl Addin for OwnedMatrixAsyncAddin {
    type AsyncExecutor = executor::Executor;
    type SharedState = State;
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(
        _: &OpenContext<Self>,
    ) -> OpenResult<Self> {
        Ok(Opened::new(State).with_async_executor(executor::Executor))
    }
}

#[excel_function(name = "TEST.OWNED.MATRIX.ASYNC")]
async fn owned_matrix(context: AsyncContext<'_, OwnedMatrixAsyncAddin>, value: Matrix<f64>) -> f64 {
    let _ = context.state();
    value.iter().copied().sum()
}

fn main() {}
