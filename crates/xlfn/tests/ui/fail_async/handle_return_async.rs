use xlfn::prelude::*;

#[derive(ExcelHandleObject)]
struct Dataset;

static __XLFN_RUNTIME: xlfn::__private::Runtime<()> =
    xlfn::__private::Runtime::new();

#[excel_function(name = "FAIL.HANDLE.ASYNC")]
async fn bad() -> Dataset {
    Dataset
}

fn main() {}
