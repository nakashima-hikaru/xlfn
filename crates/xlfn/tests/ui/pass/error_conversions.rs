use xlfn::prelude::*;

#[excel_addin(
    name = "Error Conversions",
    id = "error-conversions",
    category = "Test"
)]
struct ErrorConversions;

impl Addin for ErrorConversions {
    type SharedState = ();
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(context: &OpenContext) -> xlfn::OpenResult<Self> {
        context.diagnostics().set_sink(Discard)?;
        Ok(Opened::new(()))
    }
}

struct Discard;

impl xlfn::diagnostics::DiagnosticSink for Discard {
    fn report(&self, _: &xlfn::diagnostics::DiagnosticEvent<'_>) {}
}

#[excel_function(name = "TEST.ERROR.EXCEL", thread_safe)]
fn excel_error(value: f64) -> Result<f64, ExcelError> {
    if value < 0.0 {
        Err(ExcelError::NotAvailable)
    } else {
        Ok(value)
    }
}

#[excel_function(name = "TEST.ERROR.CUSTOM", thread_safe)]
fn custom_error() -> XllResult<f64> {
    Err(XllError::custom(
        ExcelError::NotAvailable,
        "application detail",
    ))
}

#[allow(dead_code)]
fn inspect_framework_context(error: &XllError) -> Option<(&'static str, i32)> {
    match error {
        XllError::WindowsApi { function, code, .. } => Some((*function, *code)),
        _ => None,
    }
}

fn main() {}
