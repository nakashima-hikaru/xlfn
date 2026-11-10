use xlfn::prelude::*;
use xlfn::value::{ExcelArrayRef, ExcelValueRef};


struct App;

#[excel_addin(name = "Borrowed Dynamic Value", id = "borrowed-dynamic-value", category = "Test")]
impl Addin for App {
    type SharedState = ();
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(_: &OpenContext<Self>) -> OpenResult<Self> {
        Ok(Opened::new(()))
    }
}

fn first_cell<'call>(array: &ExcelArrayRef<'call>) -> XllResult<Option<ExcelCellRef<'call>>> {
    array.get(0, 0)
}

fn text_for_call<'call>(array: &ExcelArrayRef<'call>) -> XllResult<Option<&'call str>> {
    Ok(match first_cell(array)? {
        Some(ExcelCellRef::String(text)) => Some(text),
        _ => None,
    })
}

#[excel_function(name = "TEST.DYNAMIC.THREAD", thread_safe)]
fn borrowed_dynamic(value: ExcelValueRef<'_>) -> XllResult<f64> {
    Ok(match value {
        ExcelValueRef::Scalar(ExcelCellRef::Number(number)) => number,
        ExcelValueRef::Scalar(_) | ExcelValueRef::Missing => 0.0,
        ExcelValueRef::Array(array) => text_for_call(&array)?.map_or(0.0, |text| text.len() as f64),
    })
}

#[excel_function(name = "TEST.DYNAMIC.MAIN")]
fn borrowed_dynamic_main(
    _: MainThreadContext<'_, App>,
    value: ExcelValueRef<'_>,
) -> XllResult<f64> {
    borrowed_dynamic(value)
}

#[cfg(feature = "handles")]
#[derive(ExcelHandleObject)]
struct Dataset(f64);

#[cfg(feature = "handles")]
#[excel_function(name = "TEST.DYNAMIC.IDENTITY", handle_reuse = inputs)]
fn borrowed_dynamic_identity(value: ExcelValueRef<'_>) -> XllResult<Dataset> {
    borrowed_dynamic(value).map(Dataset)
}

fn main() {}
