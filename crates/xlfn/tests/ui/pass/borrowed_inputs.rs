use xlfn::prelude::*;

struct State;

struct BorrowedInputsAddin;

#[excel_addin(name = "Borrowed Inputs", id = "borrowed-inputs", category = "Test")]
impl Addin for BorrowedInputsAddin {
    type SharedState = State;
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(_: &OpenContext<Self>) -> Result<Opened<Self::SharedState, Self::LifecycleState, Self::Layers>, Self::Error> {
        Ok(Opened::new(State))
    }
}

#[excel_function(name = "TEST.BORROWED.TEXT", thread_safe)]
fn borrowed_text(value: &str) -> f64 {
    value.chars().count() as f64
}

#[excel_function(name = "TEST.BORROWED.MATRIX", thread_safe)]
fn borrowed_matrix(value: MatrixRef<'_, f64>) -> f64 {
    value.get(0, 0).copied().unwrap_or_default() + value.iter().skip(1).copied().sum::<f64>()
}

fn matrix_element_for_call<'call>(value: &MatrixRef<'call, f64>) -> Option<&'call f64> {
    value.get(0, 0)
}

#[excel_function(name = "TEST.BORROWED.NUMERIC", thread_safe)]
fn borrowed_numeric(value: NumericMatrixRef<'_>, factor: f64) -> f64 {
    value.iter().copied().sum::<f64>() * factor
}

macro_rules! forwarded_numeric {
    ($ty:ty) => {
        #[excel_function(name = "TEST.BORROWED.NUMERIC.FORWARDED", thread_safe)]
        fn forwarded_numeric(value: $ty) -> f64 {
            value.iter().copied().sum()
        }
    };
}

forwarded_numeric!(NumericMatrixRef<'_>);

#[excel_function(name = "TEST.BORROWED.NUMERIC.ELIDED", thread_safe)]
fn elided_numeric(value: NumericMatrixRef) -> f64 {
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
