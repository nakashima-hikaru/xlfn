use xlfn_macros::excel_addin;

#[excel_addin(id = "../NUL")]
impl Addin for App {}

fn main() {}
