use xlfn::prelude::*;

#[derive(ExcelHandleObject)]
struct Dataset;

static __XLFN_RUNTIME: xlfn::__private::v1::MacroRuntime<()> =
    xlfn::__private::v1::MacroRuntime::new();

#[excel_function(name = "FAIL.HANDLE.THREADSAFE", thread_safe)]
fn bad() -> Dataset {
    Dataset
}

fn main() {}
