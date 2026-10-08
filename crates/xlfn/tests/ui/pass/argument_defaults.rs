use xlfn::prelude::*;

#[excel_addin(name = "Argument Defaults", id = "argument-defaults", category = "Test")]
struct TestAddin;

impl Addin for TestAddin {
    type SharedState = ();
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();
    #[cfg(feature = "async")]
    type AsyncExecutor = xlfn::NoAsyncExecutor;

    fn open(_: &OpenContext) -> OpenResult<Self> {
        Ok(Opened::new(()))
    }
}

#[excel_function]
fn implicit(#[excel_arg(default = 1.0)] value: f64) -> f64 {
    value
}

#[excel_function]
fn explicit(#[excel_arg(default = 1.0, missing = error, blank = default)] value: f64) -> f64 {
    value
}

#[excel_function]
fn borrowed(#[excel_arg(default = "fallback", blank = "error")] value: &str) -> String {
    value.to_owned()
}

#[excel_function]
fn optional_blank(
    #[excel_arg(default = Some(1.0), blank = default, missing = convert)] value: Option<f64>,
) -> f64 {
    value.unwrap_or(0.0)
}

#[excel_function]
fn string_convert(
    #[excel_arg(default = Some(1.0), blank = "default", missing = "convert")] value: Option<f64>,
) -> f64 {
    value.unwrap_or(0.0)
}

fn main() {
    use xlfn::__private::v1::{XLOPER12, argument_from_raw, with_excel_call_scope};
    let mut missing = XLOPER12::missing();
    let value: Option<f64> = with_excel_call_scope(|scope| {
        // SAFETY: the missing cell remains live throughout this input scope.
        unsafe { argument_from_raw(scope, "value", &mut missing) }.unwrap()
    });
    assert_eq!(value, None);
    assert_eq!(optional_blank(value), 0.0);
    assert_eq!(string_convert(value), 0.0);
}
