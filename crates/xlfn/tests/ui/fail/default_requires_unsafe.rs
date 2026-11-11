use xlfn::prelude::*;
struct TestAddin;
#[excel_addin(name = "Default Safety", id = "default-safety", category = "Test")]
impl Addin for TestAddin {
    type SharedState = ();
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();
    fn open(_: &OpenContext<Self>) -> OpenResult<Self> { Ok(Opened::new(())) }
}
unsafe fn unsafe_default() -> f64 { 1.0 }
#[excel_function]
fn unsafe_expression(#[excel_arg(default = unsafe_default())] value: f64) -> f64 { value }
#[excel_function]
fn raw_names_are_private(#[excel_arg(default = { let _ = __raw_0; 1.0 })] value: f64) -> f64 { value }
fn main() {}
