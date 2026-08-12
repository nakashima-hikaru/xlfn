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
    type State = ();
    type Error = XllError;

    fn open(_context: &OpenContext) -> Result<Self::State, Self::Error> {
        Ok(())
    }
}

#[excel_function(crate = "my_xlfn")]
fn add_numbers(a: f64, b: f64) -> f64 {
    a + b
}

fn main() {
    let _ = add_numbers(1.0, 2.0);
}
