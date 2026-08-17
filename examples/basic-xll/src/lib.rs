#![deny(unsafe_code)]

use xlfn::prelude::*;
use xlfn::reference::ExcelReference;

pub struct ExampleState;

#[excel_addin(name = "Example Add-in", id = "example-addin", category = "Example")]
pub struct ExampleAddin;

impl Addin for ExampleAddin {
    type State = ExampleState;
    type Error = XllError;

    fn open(_context: &OpenContext) -> Result<Self::State, Self::Error> {
        Ok(ExampleState)
    }
}

#[excel_function(name = "EXAMPLE.ADD", thread_safe)]
pub fn add(
    #[excel_arg(description = "First addend.")] x: f64,
    #[excel_arg(description = "Second addend.")] y: f64,
) -> XllResult<f64> {
    Ok(x + y)
}

#[excel_function(name = "EXAMPLE.GREET", thread_safe)]
pub fn greet(
    #[excel_context(thread_safe)] _context: ThreadSafeContext<'_, ExampleState>,
    name: String,
) -> XllResult<String> {
    Ok(format!("Hello, {name}!"))
}

#[excel_function(name = "EXAMPLE.ONE", thread_safe)]
pub fn one() -> i32 {
    1
}

#[excel_function(name = "EXAMPLE.REF.AREAS")]
pub fn reference_area_count(
    #[excel_context(macro_sheet)] _context: MacroSheetContext<'_, '_, ExampleState>,
    #[excel_arg(reference, description = "Cell or range reference.")] reference: ExcelReference<'_>,
) -> XllResult<i32> {
    i32::try_from(reference.areas().count()).map_err(|_| XllError::Domain {
        code: xlfn::error::DomainErrorCode::Overflow,
    })
}
