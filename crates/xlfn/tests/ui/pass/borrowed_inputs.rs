use xlfn::prelude::*;

struct State;

#[excel_addin(name = "Borrowed Inputs", id = "borrowed-inputs", category = "Test")]
struct BorrowedInputsAddin;

impl Addin for BorrowedInputsAddin {
    type SharedState = State;
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(_: &OpenContext) -> Result<Opened<Self::SharedState, Self::LifecycleState, Self::Layers>, Self::Error> {
        Ok(Opened::new(State, (), ()))
    }
}

#[excel_function(name = "TEST.BORROWED.TEXT", thread_safe)]
fn borrowed_text(value: &str) -> f64 {
    value.chars().count() as f64
}

#[excel_function(name = "TEST.BORROWED.MATRIX", thread_safe)]
fn borrowed_matrix(value: MatrixRef<'_, f64>) -> f64 {
    value.iter().copied().sum()
}

#[excel_function(name = "TEST.BORROWED.CELL", thread_safe)]
fn borrowed_cell(value: ExcelCellRef<'_>) -> f64 {
    match value {
        ExcelCellRef::Number(value) => value,
        ExcelCellRef::Boolean(value) => if value { 1.0 } else { 0.0 },
        ExcelCellRef::String(value) => value.len() as f64,
        ExcelCellRef::Error(_) | ExcelCellRef::Blank => 0.0,
    }
}

fn main() {}
