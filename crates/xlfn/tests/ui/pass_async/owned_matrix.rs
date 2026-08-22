use xlfn::prelude::*;

struct State;

#[excel_addin(name = "Owned Matrix Async", id = "owned-matrix-async", category = "Test")]
struct OwnedMatrixAsyncAddin;

impl Addin for OwnedMatrixAsyncAddin {
    type SharedState = State;
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(_: &OpenContext) -> Result<Opened<Self::SharedState, Self::LifecycleState, Self::Layers>, Self::Error> {
        Ok(Opened::new(State, (), ()))
    }
}

#[excel_function(name = "TEST.OWNED.MATRIX.ASYNC")]
async fn owned_matrix(
    #[excel_context(asynchronous)] context: AsyncContext<'_, OwnedMatrixAsyncAddin>,
    value: Matrix<f64>,
) -> f64 {
    let _ = context.state();
    value.iter().copied().sum()
}

fn main() {}
