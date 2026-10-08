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
    #[cfg(feature = "async")]
    type AsyncExecutor = xlfn::NoAsyncExecutor;

    fn open(_: &OpenContext) -> Result<Opened<Self::SharedState, Self::LifecycleState, Self::Layers>, Self::Error> {
        Ok(Opened::new(State))
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

impl<'call> xlfn::value::PrepareExcel<'call> for Positive {
    type Prepared = Box<Self>;
    fn prepare(value: xlfn::value::XlValueRef<'call>, argument: &'static str, identity: &mut xlfn::value::InputIdentityEncoder) -> xlfn::XllResult<Self::Prepared> {
        <Self as xlfn::value::FromExcel>::from_excel_with_identity(value, argument, identity).map(Box::new)
    }
    fn materialize(value: Self::Prepared) -> xlfn::XllResult<Self> { Ok(*value) }
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

// A custom owned result with a call-borrowed prepared representation.
struct Text(String);
impl<'call> FromExcel<'call> for Text {
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        String::from_excel(value, argument).map(Self)
    }
}
impl ExcelInputIdentity for Text {
    fn encode_input_identity(&self, encoder: &mut InputIdentityEncoder) {
        self.0.encode_input_identity(encoder);
    }
}
impl<'call> xlfn::value::PrepareExcel<'call> for Text {
    type Prepared = <String as xlfn::value::PrepareExcel<'call>>::Prepared;
    fn prepare(value: XlValueRef<'call>, argument: &'static str, identity: &mut InputIdentityEncoder) -> XllResult<Self::Prepared> {
        <String as xlfn::value::PrepareExcel>::prepare(value, argument, identity)
    }
    fn materialize(value: Self::Prepared) -> XllResult<Self> {
        <String as xlfn::value::PrepareExcel>::materialize(value).map(Self)
    }
}

#[excel_function(name = "TEST.CUSTOM.PREPARED.CONTAINERS")]
fn prepared_containers(
    numeric: Option<Matrix<Positive>>,
    vector: Vec<Positive>,
    row: xlfn::value::Row<Positive>,
    column: xlfn::value::Column<Positive>,
    text: Option<Matrix<Text>>,
    optional: xlfn::value::OptionalExcelValue<Matrix<Text>>,
    bounded: xlfn::value::BoundedVarArgs<Positive, 8>,
) -> PositiveHandle {
    let _ = (numeric, vector, row, column, text, optional, bounded);
    PositiveHandle
}

fn main() {}
