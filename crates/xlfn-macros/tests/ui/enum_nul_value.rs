#[derive(xlfn_macros::ExcelEnum)]
enum Value {
    #[excel_value(name = "invalid\0value")]
    Item,
}

fn main() {}
