#[path = "../async_executor.rs"]
mod executor;

use xlfn::prelude::*;
use xlfn::value::ExcelValueRef;

struct App;

#[excel_addin(name = "Borrowed Dynamic Async", id = "borrowed-dynamic-async", category = "Test")]
impl Addin for App {
    type AsyncExecutor = executor::Executor;
    type SharedState = ();
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(_: &OpenContext<Self>) -> OpenResult<Self> {
        Ok(Opened::new(()).with_async_executor(executor::Executor))
    }
}

type DynamicInput<'call> = ExcelValueRef<'call>;

#[excel_function(name = "TEST.DYNAMIC.ASYNC")]
async fn borrowed_dynamic(value: DynamicInput<'_>) -> f64 {
    match value {
        ExcelValueRef::Missing => 0.0,
        _ => 1.0,
    }
}

fn main() {}
