#[path = "../async_executor.rs"]
mod executor;

use xlfn::prelude::*;

struct TestAddin;

#[excel_addin(name = "Native ABI", id = "native-abi-async", category = "Test")]
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

#[excel_function]
async fn native(number: f64, enabled: bool, count: i32, text: String) -> f64 {
    std::future::ready(()).await;
    number + f64::from(count) + if enabled { text.len() as f64 } else { 0.0 }
}

#[excel_function]
async fn strict(
    #[excel_arg(strict)] number: f64,
    #[excel_arg(strict)] enabled: bool,
    #[excel_arg(strict)] count: i32,
    #[excel_arg(strict)] text: String,
) -> f64 {
    native(number, enabled, count, text).await
}

#[excel_function]
async fn presence(#[excel_arg(default = 1.0)] number: f64) -> f64 {
    number
}

fn main() {
    use xlfn::__private::v1::XLOPER12;
    type Value = *mut XLOPER12;
    let _: unsafe extern "system" fn(f64, i16, i32, *const u16, Value) = xll_native;
    let _: unsafe extern "system" fn(Value, Value, Value, Value, Value) = xll_strict;
    let _: unsafe extern "system" fn(Value, Value) = xll_presence;
}
