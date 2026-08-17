use xlfn::prelude::*;
use xlfn::value::XlArrayRef;

struct State;

#[excel_addin(name = "Borrowed Async", id = "borrowed-async", category = "Test")]
struct BorrowedAsyncAddin;

impl Addin for BorrowedAsyncAddin {
    type State = State;
    type Error = XllError;

    fn open(_: &OpenContext) -> Result<Self::State, Self::Error> {
        Ok(State)
    }
}

#[excel_function(name = "FAIL.BORROWED.ASYNC")]
async fn bad(values: XlArrayRef<'_>) -> f64 {
    values.len() as f64
}

fn main() {}
