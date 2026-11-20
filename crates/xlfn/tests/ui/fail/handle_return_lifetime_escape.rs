use xlfn::prelude::*;

#[derive(ExcelHandleObject)]
struct Dataset;

static __XLFN_RUNTIME: xlfn::__private::v1::MacroRuntime<()> =
    xlfn::__private::v1::MacroRuntime::new();

#[excel_function]
fn escape<'call>(value: Handle<'call, Dataset>) -> HandleAlias<'static, Dataset> {
    value.alias()
}

#[excel_function]
fn wrong_input<'selected, 'other>(
    _selected: Handle<'selected, Dataset>,
    other: Handle<'other, Dataset>,
) -> HandleAlias<'selected, Dataset> {
    other.alias()
}

fn main() {}
