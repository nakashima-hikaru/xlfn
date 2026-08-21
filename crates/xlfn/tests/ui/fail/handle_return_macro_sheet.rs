use xlfn::prelude::*;

#[derive(ExcelHandleObject)]
struct Dataset;

static __XLFN_RUNTIME: xlfn::macro_support::MacroRuntime<()> =
    xlfn::macro_support::MacroRuntime::new();

#[excel_function(name = "FAIL.HANDLE.MACRO", macro_sheet)]
fn bad() -> Dataset {
    Dataset
}

fn main() {}
