use xlfn::prelude::*;

struct State;

#[excel_addin(name = "Async Compile Test", id = "async-compile-test", category = "Test")]
struct AsyncTestAddin;

impl Addin for AsyncTestAddin {
    type State = State;
    type Error = XllError;

    fn open(_: &OpenContext) -> Result<Self::State, Self::Error> {
        Ok(State)
    }
}

#[derive(ExcelHandleObject)]
struct Dataset {
    size: f64,
}

type DatasetHandle = Handle<Dataset>;

#[excel_function(name = "TEST.HANDLE.ASYNC")]
async fn async_handle_input(
    #[excel_context(asynchronous)] context: AsyncContext<State>,
    dataset: DatasetHandle,
) -> XllResult<f64> {
    let _ = context.state();
    Ok(dataset.size)
}

fn main() {}
