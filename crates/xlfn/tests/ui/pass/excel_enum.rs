use xlfn::prelude::*;

struct State;

#[excel_addin(name = "Excel Enum Compile Test", id = "excel-enum-compile-test", category = "Test")]
struct TestAddin;

impl Addin for TestAddin {
    type SharedState = State;
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(_: &OpenContext) -> Result<Opened<Self::SharedState, Self::LifecycleState, Self::Layers>, Self::Error> {
        Ok(Opened::new(State))
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

mod shadowed_result {
    use Status::*;

    #[derive(Clone, Copy, Debug, PartialEq, xlfn::ExcelEnum)]
    pub enum Status {
        Ok,
        Failed,
    }

    pub fn check_materialize() {
        for status in [Ok, Failed] {
            assert_eq!(
                <Status as xlfn::value::PrepareExcel<'static>>::materialize(status).unwrap(),
                status,
            );
        }
    }
}

fn main() {
    shadowed_result::check_materialize();
}
