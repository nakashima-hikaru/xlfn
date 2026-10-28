use xlfn::output::{NumericArrayBuilder, NumericArrayOutput};
use xlfn::prelude::*;

struct State;

#[excel_addin(name = "Numeric Return", id = "numeric-return", category = "Test")]
struct NumericReturnAddin;

impl Addin for NumericReturnAddin {
    type RtdSubscription = ();
    type RtdSources = xlfn::DynamicRtdSources<Self::RtdSubscription>;
    type SharedState = State;
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();
    #[cfg(feature = "async")]
    type AsyncExecutor = xlfn::NoAsyncExecutor;

    fn open(_: &OpenContext<Self>) -> Result<Opened<State, (), ()>, XllError> {
        Ok(Opened::new(State))
    }
}

#[excel_function(thread_safe)]
fn numeric_matrix(spot: f64, strikes: NumericMatrixRef<'_>) -> XllResult<Matrix<f64>> {
    Matrix::new(
        strikes.rows(),
        strikes.columns(),
        strikes.iter().map(|strike| spot + strike).collect(),
    )
}

#[excel_function(thread_safe)]
fn numeric_row() -> XllResult<Row<f64>> {
    Row::new(vec![1.0, 2.0])
}

#[excel_function(thread_safe)]
fn numeric_column() -> XllResult<Column<f64>> {
    Column::new(vec![1.0, 2.0])
}

#[excel_function(thread_safe)]
fn numeric_output() -> XllResult<NumericArrayOutput> {
    let mut output = NumericArrayBuilder::new(1, 1)?;
    output.push_f64(1.0)?;
    output.finish()
}

type NumericAlias = XllResult<Matrix<f64>>;

#[excel_function(thread_safe)]
fn alias_default() -> NumericAlias {
    Matrix::new(1, 1, vec![1.0])
}

#[excel_function(thread_safe, return_abi = "fp12")]
fn alias_explicit() -> NumericAlias {
    Matrix::new(1, 1, vec![1.0])
}

#[excel_function(thread_safe, return_abi = "xloper12")]
fn explicit_xloper12() -> XllResult<Matrix<f64>> {
    Matrix::new(1, 1, vec![1.0])
}

#[excel_function(thread_safe)]
fn integer_matrix() -> XllResult<Matrix<i32>> {
    Matrix::new(1, 1, vec![1])
}

macro_rules! forwarded_numeric_return {
    ($result:ty) => {
        #[excel_function(thread_safe)]
        fn forwarded_numeric_return() -> $result {
            Matrix::new(1, 1, vec![1.0])
        }
    };
}

forwarded_numeric_return!(XllResult<Matrix<f64>>);

#[cfg(feature = "async")]
#[excel_function]
async fn async_numeric_matrix() -> XllResult<Matrix<f64>> {
    Matrix::new(1, 1, vec![1.0])
}

type Fp12 = xlfn::__private::v1::FP12;
type Xloper12 = xlfn::__private::v1::XLOPER12;

const _: unsafe extern "system" fn(f64, *const Fp12) -> *mut Fp12 = xll_numeric_matrix;
const _: unsafe extern "system" fn() -> *mut Fp12 = xll_numeric_row;
const _: unsafe extern "system" fn() -> *mut Fp12 = xll_numeric_column;
const _: unsafe extern "system" fn() -> *mut Fp12 = xll_numeric_output;
const _: unsafe extern "system" fn() -> *mut Fp12 = xll_alias_explicit;
const _: unsafe extern "system" fn() -> *mut Fp12 = xll_forwarded_numeric_return;
const _: unsafe extern "system" fn() -> *mut Xloper12 = xll_alias_default;
const _: unsafe extern "system" fn() -> *mut Xloper12 = xll_explicit_xloper12;
const _: unsafe extern "system" fn() -> *mut Xloper12 = xll_integer_matrix;
const _: extern "system" fn() = __xlfn_calculation_ended;
#[cfg(feature = "async")]
const _: unsafe extern "system" fn(*mut Xloper12) = xll_async_numeric_matrix;

fn main() {}
