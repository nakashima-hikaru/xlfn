use xlfn::prelude::*;
use xlfn::value::{FromExcel, XlValueRef};

struct CustomRate(f64);

impl<'call> FromExcel<'call> for CustomRate {
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        value.as_f64().map(Self).map_err(|error| match error {
            XllError::Input { reason, .. } => XllError::Input { argument, reason },
            other => other,
        })
    }
}

#[derive(ExcelHandleObject)]
struct Dataset;

static __XLFN_RUNTIME: xlfn::__private::v1::MacroRuntime<()> =
    xlfn::__private::v1::MacroRuntime::new();

#[excel_function(name = "FAIL.FORMULA.IDENTITY")]
fn bad(value: CustomRate) -> Dataset {
    let _ = value;
    Dataset
}

fn main() {}
