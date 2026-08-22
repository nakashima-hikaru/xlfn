use xlfn::prelude::*;

struct State;

#[excel_addin(name = "Owned Matrix Async", id = "owned-matrix-async", category = "Test")]
struct OwnedMatrixAsyncAddin;

impl Addin for OwnedMatrixAsyncAddin {
    type State = State;
    type Error = XllError;
    type Layers = ();

    fn open(_: &OpenContext) -> Result<Opened<Self::State, Self::Layers>, Self::Error> {
        Ok(Opened::new(State, ()))
    }
}

#[excel_function(name = "TEST.OWNED.MATRIX.ASYNC")]
async fn owned_matrix(value: Matrix<f64>) -> f64 {
    value.iter().copied().sum()
}

fn main() {}
