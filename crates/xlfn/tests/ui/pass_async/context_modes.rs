use xlfn::prelude::*;

#[excel_addin(name = "Context Async", id = "context-async", category = "Test")]
struct TestAddin;

impl Addin for TestAddin {
    type SharedState = ();
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();
    #[cfg(feature = "async")]
    type AsyncExecutor = xlfn::NoAsyncExecutor;

    fn open(_: &OpenContext) -> OpenResult<Self> {
        Ok(Opened::new(()))
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

fn main() {
    type AsyncExport = unsafe extern "system" fn(
        *mut xlfn::__private::v1::XLOPER12,
        *mut xlfn::__private::v1::XLOPER12,
    );

    // Every entrypoint has one visible input and Excel's async completion handle.
    let _: AsyncExport = xll_context;
    let _: AsyncExport = xll_qualified_context;
    let _: AsyncExport = xll_no_context;
}
