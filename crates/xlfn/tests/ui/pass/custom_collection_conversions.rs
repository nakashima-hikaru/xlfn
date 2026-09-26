use xlfn::prelude::*;
use xlfn::value::{convert, FromExcel, Row, Column, XlStrRef, XlValueRef};

#[excel_addin(name = "Collection Conversion", id = "collection-conversion", category = "Test")]
struct CollectionConversionAddin;

impl Addin for CollectionConversionAddin {
    type SharedState = ();
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(_: &OpenContext) -> XllResult<Opened<()>> {
        Ok(Opened::new(()))
    }
}

struct NumericMatrix(Matrix<f64>);

impl<'call> FromExcel<'call> for NumericMatrix {
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        convert::matrix(value, argument, f64::from_excel).map(Self)
    }
}

struct OptionalMatrix(Option<Matrix<f64>>);

impl<'call> FromExcel<'call> for OptionalMatrix {
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        convert::optional(value, argument, |value, argument| {
            convert::matrix(value, argument, f64::from_excel)
        }).map(Self)
    }
}

struct NumericVector(Vec<f64>);

impl<'call> FromExcel<'call> for NumericVector {
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        convert::vector(value, argument, f64::from_excel).map(Self)
    }
}

struct NumericColumn(Column<f64>);

impl<'call> FromExcel<'call> for NumericColumn {
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        convert::column(value, argument, f64::from_excel).map(Self)
    }
}

struct TextRow<'call>(Row<XlStrRef<'call>>);

impl<'call> FromExcel<'call> for TextRow<'call> {
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        convert::row(value, argument, XlValueRef::as_str_with_argument).map(Self)
    }
}

#[excel_function(name = "TEST.CUSTOM.COLLECTIONS", thread_safe)]
fn collections(matrix: NumericMatrix, optional: OptionalMatrix, vector: NumericVector, column: NumericColumn) -> f64 {
    matrix.0.iter().sum::<f64>()
        + optional.0.map_or(0.0, |value| value.iter().sum())
        + vector.0.iter().sum::<f64>()
        + column.0.as_slice().iter().sum::<f64>()
}

#[excel_function(name = "TEST.CUSTOM.TEXT.ROW", thread_safe)]
fn text_row(value: TextRow<'_>) -> f64 {
    value.0.as_slice().iter().map(|text| text.as_utf16().len() as f64).sum()
}

fn main() {}
