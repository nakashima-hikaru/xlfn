use xlfn::prelude::*;
use xlfn::{unstable::output::{XlArrayBuilder, XlArrayOutput}, value::XlArrayRef};

struct State;

#[excel_addin(name = "Borrowed Array", id = "borrowed-array", category = "Test")]
struct BorrowedArrayAddin;

impl Addin for BorrowedArrayAddin {
    type SharedState = State;
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(_: &OpenContext) -> Result<Opened<Self::SharedState, Self::LifecycleState, Self::Layers>, Self::Error> {
        Ok(Opened::new(State, (), ()))
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
