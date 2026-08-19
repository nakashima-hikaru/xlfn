use xlfn::prelude::*;

struct State;

#[excel_addin(name = "Case Test", id = "case-test", category = "Test")]
struct CaseAddin;

impl Addin for CaseAddin {
    type State = State;
    type Error = XllError;
    type Layers = ();

    fn open(_: &OpenContext) -> Result<Self::State, Self::Error> {
        Ok(State)
    }

    fn udf_layers(_: &Self::State) -> Self::Layers {}
}

#[excel_function(id = "lower_foo", name = "TEST.LOWER")]
fn foo() -> f64 {
    1.0
}

#[allow(non_snake_case)]
#[excel_function(id = "upper_foo", name = "TEST.UPPER")]
fn FOO() -> f64 {
    2.0
}

fn main() {
    assert_eq!(foo(), 1.0);
    assert_eq!(FOO(), 2.0);
}
