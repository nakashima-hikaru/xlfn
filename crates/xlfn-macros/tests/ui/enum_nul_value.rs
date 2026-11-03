#[derive(xlfn_macros::ExcelEnum)]
enum Value {
    #[excel_enum(name = "invalid\0value")]
    Item,
}

fn main() {}
