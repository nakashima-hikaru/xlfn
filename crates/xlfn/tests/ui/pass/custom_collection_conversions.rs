use xlfn::prelude::*;
use xlfn::value::{
    Column, ExcelInputIdentity, FromExcel, InputIdentityEncoder, PrepareExcel, Row, XlStrRef,
    XlValueRef, convert,
};

#[excel_addin(
    name = "Collection Conversion",
    id = "collection-conversion",
    category = "Test"
)]
struct CollectionConversionAddin;

impl Addin for CollectionConversionAddin {
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

struct NumericMatrix(Matrix<f64>);

impl<'call> FromExcel<'call> for NumericMatrix {
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        convert::matrix(value, argument, f64::from_excel).map(Self)
    }
}

// Custom identity and preparation contracts remain usable in core-only builds.
// Exercise the complete encoder API without requiring a handle-producing UDF.
impl ExcelInputIdentity for NumericMatrix {
    fn encode_input_identity(&self, encoder: &mut InputIdentityEncoder) {
        encoder.tag(1);
        encoder.bytes(b"numeric-matrix");
        encoder.string("row-major");
        encoder.bool(true);
        encoder.u32(self.0.rows() as u32);
        encoder.u64(self.0.columns() as u64);
        encoder.i64(0);
        for value in self.0.iter() {
            encoder.f64(*value);
        }
    }
}

impl<'call> PrepareExcel<'call> for NumericMatrix {
    type Prepared = Self;

    fn prepare(
        value: XlValueRef<'call>,
        argument: &'static str,
        identity: &mut InputIdentityEncoder,
    ) -> XllResult<Self::Prepared> {
        Self::from_excel_with_identity(value, argument, identity)
    }

    fn materialize(prepared: Self::Prepared) -> XllResult<Self> {
        Ok(prepared)
    }
}

struct OptionalMatrix(Option<Matrix<f64>>);

impl<'call> FromExcel<'call> for OptionalMatrix {
    fn from_excel(value: XlValueRef<'call>, argument: &'static str) -> XllResult<Self> {
        convert::optional(value, argument, |value, argument| {
            convert::matrix(value, argument, f64::from_excel)
        })
        .map(Self)
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
fn collections(
    matrix: NumericMatrix,
    optional: OptionalMatrix,
    vector: NumericVector,
    column: NumericColumn,
) -> f64 {
    matrix.0.iter().sum::<f64>()
        + optional.0.map_or(0.0, |value| value.iter().sum())
        + vector.0.iter().sum::<f64>()
        + column.0.as_slice().iter().sum::<f64>()
}

#[excel_function(name = "TEST.CUSTOM.TEXT.ROW", thread_safe)]
fn text_row(value: TextRow<'_>) -> f64 {
    value
        .0
        .as_slice()
        .iter()
        .map(|text| text.as_utf16().len() as f64)
        .sum()
}

fn main() {
    fn requires_send_sync<T: Send + Sync>() {}
    requires_send_sync::<InputIdentityEncoder>();
}
