#[path = "../async_executor.rs"]
mod executor;
use xlfn::prelude::*;
struct TestAddin;
#[excel_addin(name = "Async Default Safety", id = "async-default-safety", category = "Test")]
impl Addin for TestAddin {
    type AsyncExecutor = executor::Executor;
    type SharedState = ();
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();
    fn open(_: &OpenContext<Self>) -> OpenResult<Self> {
        Ok(Opened::new(()).with_async_executor(executor::Executor))
    }
}
unsafe fn unsafe_default() -> f64 { 1.0 }
#[excel_function]
async fn unsafe_expression(#[excel_arg(default = unsafe_default())] value: f64) -> f64 { value }
fn main() {}
