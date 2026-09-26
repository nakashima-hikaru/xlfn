use xlfn::prelude::*;
use xlfn::reference::{ExcelReference, FromExcelReference};
use xlfn::value::{ExcelInputIdentity, InputIdentityEncoder, XlValueRef};

#[derive(ExcelHandleObject)]
struct Dataset;

struct Reference<'call>(ExcelReference<'call>);

impl<'call> FromExcelReference<'call> for Reference<'call> {
    fn from_excel_reference(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        ExcelReference::from_excel_reference(value, argument).map(Self)
    }
}

impl ExcelInputIdentity for Reference<'_> {
    fn encode_input_identity(&self, encoder: &mut InputIdentityEncoder) {
        encoder.u64(self.0.areas().count() as u64);
    }
}

static __XLFN_RUNTIME: xlfn::__private::v1::MacroRuntime<()> =
    xlfn::__private::v1::MacroRuntime::new();

#[excel_function(name = "FAIL.REFERENCE.HANDLE", macro_sheet)]
fn bad(#[excel_arg(reference)] _reference: Reference<'_>) -> Dataset {
    Dataset
}

fn main() {}
