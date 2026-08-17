use xlfn::prelude::*;
use xlfn::value::XlArrayRef;

type BorrowedArray<'call> = XlArrayRef<'call>;

#[excel_addin(name = "Borrowed Async Alias", id = "borrowed-async-alias", category = "Test")]
struct BorrowedAsyncAliasAddin;

impl Addin for BorrowedAsyncAliasAddin {
    type State = ();
    type Error = XllError;

    fn open(_: &OpenContext) -> Result<Self::State, Self::Error> {
        Ok(())
    }
}

#[excel_function(name = "FAIL.BORROWED.ASYNC.ALIAS")]
async fn bad(values: BorrowedArray<'_>) -> f64 {
    values.len() as f64
}

fn main() {}
