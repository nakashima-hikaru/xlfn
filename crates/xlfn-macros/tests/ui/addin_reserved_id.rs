use xlfn_macros::excel_addin;

#[excel_addin(id = "COM1")]
impl Addin for App {}

fn main() {}
