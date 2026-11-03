use xlfn::prelude::*;
struct TestAddin;
#[excel_addin(name = "Disabled Executor", id = "disabled-executor")]
impl Addin for TestAddin {
    type SharedState = ();
    fn open(_: &OpenContext<Self>) -> OpenResult<Self> { Ok(Opened::new(())) }
}
#[excel_function]
async fn value() -> f64 { 1.0 }
fn main() {}
