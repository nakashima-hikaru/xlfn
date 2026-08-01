use xlfn::prelude::*;

static __XLFN_RUNTIME: xlfn::__private::Runtime<()> =
    xlfn::__private::Runtime::new();

#[excel_function(name = "FAIL.ASYNC.FEATURE")]
async fn bad() -> f64 {
    0.0
}

fn main() {}
