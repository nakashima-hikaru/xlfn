use xlfn::prelude::*;
use xlfn::value::{FromExcel, XlValueRef};

struct TestAddin;
#[excel_addin(name = "Eager Inputs", id = "eager-inputs")]
impl Addin for TestAddin {
    type SharedState = ();
    fn open(_: &OpenContext<Self>) -> OpenResult<Self> { Ok(Opened::new(())) }
}

#[derive(ExcelInputIdentity, ExcelPrepare)]
struct Positive(f64);
impl<'call> FromExcel<'call> for Positive {
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        let number = f64::from_excel(value, argument)?;
        if number < 0.0 {
            return Err(XllError::input(argument, xlfn::error::InputError::OutOfRange));
        }
        Ok(Self(number))
    }
}
#[derive(ExcelInputIdentity)]
struct Shape<T> { values: Matrix<T>, label: String }
#[derive(ExcelInputIdentity)]
enum Choice<T> { Empty, Value(T), Named { value: T } }
#[derive(ExcelInputIdentity, ExcelPrepare)]
struct Generic<T>(T);
impl<'call, T: FromExcel<'call>> FromExcel<'call> for Generic<T> {
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        T::from_excel(value, argument).map(Self)
    }
}

#[excel_function(name = "TEST.EAGER")]
fn eager(value: Positive) -> f64 { value.0 }
fn require_preparation<'call, T: xlfn::value::PrepareExcel<'call>>() {}
fn main() { require_preparation::<Positive>(); require_preparation::<Generic<f64>>(); }
