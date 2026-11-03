use xlfn::prelude::*;

struct TestAddin;
#[excel_addin(name = "Policy", id = "policy")]
impl Addin for TestAddin {
    type SharedState = ();
    fn open(_: &OpenContext<Self>) -> OpenResult<Self> {
        Ok(Opened::new(()))
    }
}

#[excel_function(handle_reuse = never)]
fn scalar() -> f64 { 1.0 }

fn main() {}
