#[path = "../async_executor.rs"]
mod executor;

use xlfn::prelude::*;

struct TestAddin;

#[excel_addin(name = "Context Async", id = "context-async", category = "Test")]
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

use xlfn::AsyncContext;

#[excel_function]
async fn context(context: AsyncContext<'_, TestAddin>, value: f64) -> f64 {
    let _ = context.state();
    std::future::ready(()).await;
    value
}

#[excel_function]
async fn qualified_context(context: xlfn::AsyncContext<'_, TestAddin>, value: f64) -> f64 {
    let _ = context.state();
    std::future::ready(()).await;
    value
}

#[excel_function]
async fn no_context(value: f64) -> f64 {
    std::future::ready(()).await;
    value
}

#[excel_function]
async fn elided_context(context: AsyncContext<TestAddin>, value: f64) -> f64 {
    let _ = context.state();
    std::future::ready(()).await;
    value
}

fn main() {
    type AsyncExport = unsafe extern "system" fn(
        f64,
        *mut xlfn::__private::v1::XLOPER12,
    );

    // Every entrypoint has one visible input and Excel's async completion handle.
    let _: AsyncExport = xll_context;
    let _: AsyncExport = xll_qualified_context;
    let _: AsyncExport = xll_no_context;
    let _: AsyncExport = xll_elided_context;
}
