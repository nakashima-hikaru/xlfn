use xlfn::prelude::*;
use xlfn::reference::{ExcelReference, FromExcelReference, ReferenceArea, SheetId};
use xlfn::value::XlValueRef;

#[excel_addin(name = "Reference Conversion", id = "reference-conversion", category = "Test")]
struct App;

impl Addin for App {
    type SharedState = ();
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();
    #[cfg(feature = "async")]
    type AsyncExecutor = xlfn::NoAsyncExecutor;

    fn open(_: &OpenContext) -> XllResult<Opened<()>> {
        Ok(Opened::new(()))
    }
}

struct ReferenceMetadata {
    first: ReferenceArea,
    sheet: Option<SheetId>,
}

impl<'call> FromExcelReference<'call> for ReferenceMetadata {
    fn from_excel_reference(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        let reference = ExcelReference::from_excel_reference(value, argument)?;
        let first = reference.areas().next().ok_or_else(|| {
            XllError::input(argument, xlfn::error::InputError::Malformed("empty reference"))
        })?;
        Ok(Self { first, sheet: reference.sheet_id() })
    }
}

struct BorrowedReference<'call>(ExcelReference<'call>);

impl<'call> FromExcelReference<'call> for BorrowedReference<'call> {
    fn from_excel_reference(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        ExcelReference::from_excel_reference(value, argument).map(Self)
    }
}

#[excel_function(name = "TEST.REFERENCE.OWNED", macro_sheet)]
fn owned(#[excel_arg(reference)] reference: ReferenceMetadata) -> f64 {
    let _sheet_identity: Option<usize> = reference.sheet.map(SheetId::get);
    f64::from(reference.first.first_row())
}

#[excel_function(name = "TEST.REFERENCE.BORROWED", macro_sheet)]
fn borrowed(#[excel_arg(reference)] reference: BorrowedReference<'_>) -> f64 {
    reference.0.areas().count() as f64
}

fn main() {}
