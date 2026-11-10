use xlfn_macros::excel_addin;

#[excel_addin(name = "First", name = "Second")]
impl Addin for App {}

fn main() {}
