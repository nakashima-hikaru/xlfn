#[path = "../async_executor.rs"]
mod executor;
use xlfn::prelude::*;

struct TestAddin;
#[excel_addin(name = "Async Numeric Return", id = "async-numeric-return")]
impl Addin for TestAddin {
    type SharedState = ();
    type AsyncExecutor = executor::Executor;
    fn open(_: &OpenContext<Self>) -> OpenResult<Self> {
        Ok(Opened::new(()).with_async_executor(executor::Executor))
    }
}

#[excel_function]
async fn async_numeric_matrix() -> XllResult<Matrix<f64>> {
    Matrix::new(1, 1, vec![1.0])
}
const _: unsafe extern "system" fn(*mut xlfn::__private::v1::XLOPER12) = xll_async_numeric_matrix;
fn main() {}
