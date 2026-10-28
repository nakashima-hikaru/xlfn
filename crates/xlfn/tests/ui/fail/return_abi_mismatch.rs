use xlfn::prelude::*;

struct State;

struct ReturnMismatchAddin;

#[excel_addin(name = "Return Mismatch", id = "return-mismatch", category = "Test")]
impl Addin for ReturnMismatchAddin {
    type SharedState = State;
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();
    #[cfg(feature = "async")]
    type AsyncExecutor = xlfn::NoAsyncExecutor;

    fn open(_: &OpenContext<Self>) -> Result<Opened<State, (), ()>, XllError> {
        Ok(Opened::new(State))
    }
}

#[excel_function(thread_safe, return_abi = "fp12")]
fn scalar() -> f64 { 1.0 }

#[excel_function(thread_safe, return_abi = "fp12")]
fn strings() -> XllResult<Matrix<String>> { Matrix::new(1, 1, vec!["one".into()]) }

fn main() {}
