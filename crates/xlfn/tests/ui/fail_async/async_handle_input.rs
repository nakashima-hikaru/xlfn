#[path = "../async_executor.rs"]
mod executor;

use xlfn::prelude::*;

struct State;

struct AsyncTestAddin;

#[excel_addin(
    name = "Async Compile Test",
    id = "async-compile-test",
    category = "Test"
)]
impl Addin for AsyncTestAddin {
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

#[derive(ExcelHandleObject)]
struct Dataset {
    size: f64,
}

#[excel_function(name = "TEST.HANDLE.ASYNC")]
async fn async_handle_input(
    context: AsyncContext<'_, AsyncTestAddin>,
    dataset: Handle<'_, Dataset>,
) -> XllResult<f64> {
    let _ = context.state();
    Ok(dataset.size)
}

fn main() {}
