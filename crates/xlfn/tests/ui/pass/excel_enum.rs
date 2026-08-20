use xlfn::prelude::*;

struct State;

#[excel_addin(name = "Excel Enum Compile Test", id = "excel-enum-compile-test", category = "Test")]
struct TestAddin;

impl Addin for TestAddin {
    type State = State;
    type Error = XllError;
    type Layers = ();

    fn open(_: &OpenContext) -> Result<Opened<Self::State, Self::Layers>, Self::Error> {
        Ok(Opened::new(State, ()))
    }
}

#[derive(Clone, Copy, ExcelEnum)]
#[excel_enum(ascii_case_insensitive)]
enum Direction {
    #[excel_value(name = "Forward")]
    Forward,
    #[excel_value(name = "Reverse")]
    Reverse,
}

#[excel_function(name = "DIRECTION.SIGN", thread_safe)]
fn sign(direction: Direction) -> f64 {
    match direction {
        Direction::Forward => 1.0,
        Direction::Reverse => -1.0,
    }
}

fn main() {}
