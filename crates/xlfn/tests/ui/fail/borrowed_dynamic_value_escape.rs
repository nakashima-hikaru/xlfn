use xlfn::__private::v1::{XLOPER12, argument_from_raw, with_excel_call_scope};
use xlfn::value::{ExcelArrayRef, ExcelCellRef, ExcelValueRef};
use xlfn::XllResult;

fn escape_array<'call>(array: ExcelArrayRef<'call>) -> ExcelArrayRef<'static> {
    array
}

fn escape_cell<'call>(array: &ExcelArrayRef<'call>) -> XllResult<Option<ExcelCellRef<'static>>> {
    array.get(0, 0)
}

fn main() {
    let mut raw = XLOPER12::missing();
    let _escaped: ExcelValueRef<'static> = with_excel_call_scope(|scope| {
        // SAFETY: the raw value remains live throughout the conversion.
        unsafe { argument_from_raw(scope, "value", &mut raw) }.unwrap()
    });
}
