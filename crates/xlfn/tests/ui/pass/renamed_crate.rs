use xlfn as my_xlfn;
use my_xlfn::prelude::*;

#[derive(ExcelEnum)]
#[excel_enum(crate = "my_xlfn")]
pub enum CustomStatus {
    Ok,
    Err,
}

#[derive(ExcelHandleObject)]
#[excel_handle(crate = "my_xlfn")]
pub struct CustomHandle {
    pub id: u64,
}

#[excel_addin(crate = "my_xlfn")]
pub struct RenamedAddin;

impl Addin for RenamedAddin {
    type SharedState = ();
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(_context: &OpenContext) -> Result<Opened<Self::SharedState, Self::LifecycleState, Self::Layers>, Self::Error> {
        Ok(Opened::new((), (), ()))
    }
}

#[excel_function(crate = "my_xlfn")]
fn add_numbers(a: f64, b: f64) -> f64 {
    a + b
}

fn main() {
    let _ = add_numbers(1.0, 2.0);
}
