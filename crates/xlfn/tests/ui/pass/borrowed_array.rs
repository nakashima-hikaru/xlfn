use xlfn::prelude::*;
use xlfn::{advanced::output::{XlArrayBuilder, XlArrayOutput}, value::XlArrayRef};

struct State;

#[excel_addin(name = "Borrowed Array", id = "borrowed-array", category = "Test")]
struct BorrowedArrayAddin;

impl Addin for BorrowedArrayAddin {
    type State = State;
    type Error = XllError;

    fn open(_: &OpenContext) -> Result<Self::State, Self::Error> {
        Ok(State)
    }
}

#[excel_function(name = "TEST.BORROWED.ARRAY", thread_safe)]
fn sum(values: XlArrayRef<'_>) -> XllResult<f64> {
    values
        .cells()
        .try_fold(0.0, |sum, cell| Ok(sum + cell.as_f64()?))
}

#[excel_function(name = "TEST.DIRECT.ARRAY", thread_safe)]
fn normalize(values: XlArrayRef<'_>) -> XllResult<XlArrayOutput> {
    let mean = values
        .cells()
        .try_fold(0.0, |sum, cell| Ok(sum + cell.as_f64()?))?
        / values.len() as f64;
    let (rows, columns) = values.shape();
    let mut output = XlArrayBuilder::new(rows, columns)?;
    for cell in values.cells() {
        output.push_f64(cell.as_f64()? - mean)?;
    }
    output.finish()
}

fn main() {}
