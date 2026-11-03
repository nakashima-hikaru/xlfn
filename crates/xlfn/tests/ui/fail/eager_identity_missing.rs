use xlfn::ExcelInputIdentity;
struct Opaque;
#[derive(ExcelInputIdentity)]
struct Custom(Opaque);
fn main() {}
