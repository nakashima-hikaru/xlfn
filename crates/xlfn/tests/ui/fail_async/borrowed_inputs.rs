use xlfn::prelude::*;

struct State;

#[excel_addin(name = "Borrowed Async Inputs", id = "borrowed-async-inputs", category = "Test")]
struct BorrowedAsyncInputsAddin;

impl Addin for BorrowedAsyncInputsAddin {
    type State = State;
    type Error = XllError;
    type Layers = ();

    fn open(_: &OpenContext) -> Result<Opened<Self::State, Self::Layers>, Self::Error> {
        Ok(Opened::new(State, ()))
    }
}

#[excel_function(name = "TEST.BORROWED.TEXT.ASYNC")]
async fn borrowed_text(value: &str) -> f64 {
    value.len() as f64
}

#[excel_function(name = "TEST.BORROWED.MATRIX.ASYNC")]
async fn borrowed_matrix(value: MatrixRef<'_, f64>) -> f64 {
    value.iter().copied().sum()
}

#[excel_function(name = "TEST.BORROWED.CELL.ASYNC")]
async fn borrowed_cell(value: ExcelCellRef<'_>) -> f64 {
    match value {
        ExcelCellRef::Number(value) => value,
        ExcelCellRef::Boolean(value) => if value { 1.0 } else { 0.0 },
        ExcelCellRef::String(value) => value.len() as f64,
        ExcelCellRef::Error(_) | ExcelCellRef::Blank => 0.0,
    }
}

fn main() {}
