use xlfn::prelude::*;
use xlfn::value::{
    ExcelCellOutput, ExcelInputIdentity, FromExcel, InputIdentityEncoder, IntoExcel, XlValueRef,
};

struct State;

#[excel_addin(name = "Custom Conversion", id = "custom-conversion", category = "Test")]
struct CustomConversionAddin;

impl Addin for CustomConversionAddin {
    type SharedState = State;
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(_: &OpenContext) -> Result<Opened<Self::SharedState, Self::LifecycleState, Self::Layers>, Self::Error> {
        Ok(Opened::new(State, (), ()))
    }
}

struct Positive(f64);

impl<'call> FromExcel<'call> for Positive {
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        let value = value.as_f64()?;
        if value < 0.0 {
            return Err(XllError::input(
                argument,
                xlfn::error::InputError::OutOfRange,
            ));
        }
        Ok(Self(value))
    }
}

impl IntoExcel for Positive {
    fn into_excel(self) -> XllResult<ExcelCellOutput> {
        self.0.into_excel()
    }
}

impl ExcelInputIdentity for Positive {
    fn encode_input_identity(&self, encoder: &mut InputIdentityEncoder) {
        encoder.f64(self.0);
    }
}

#[derive(ExcelHandleObject)]
struct PositiveHandle;

#[excel_function(name = "TEST.CUSTOM.CONVERSION", thread_safe)]
fn custom_conversion(value: Positive) -> Positive {
    value
}

#[excel_function(name = "TEST.CUSTOM.MATRIX", thread_safe)]
fn custom_matrix(value: Positive) -> XllResult<Matrix<Positive>> {
    Matrix::new(1, 1, vec![value])
}

#[excel_function(name = "TEST.CUSTOM.HANDLE")]
fn custom_handle(value: Positive) -> PositiveHandle {
    let _ = value;
    PositiveHandle
}

fn main() {}
