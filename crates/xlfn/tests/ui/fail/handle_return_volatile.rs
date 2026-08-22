use xlfn::prelude::*;

#[derive(ExcelHandleObject)]
struct Dataset;

static __XLFN_RUNTIME: xlfn::__private::MacroRuntime<()> =
    xlfn::__private::MacroRuntime::new();

#[excel_function(name = "FAIL.HANDLE.VOLATILE", volatile)]
fn bad() -> Dataset {
    Dataset
}

fn main() {}
