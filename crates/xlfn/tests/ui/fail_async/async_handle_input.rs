use xlfn::prelude::*;

struct State;

#[excel_addin(name = "Async Compile Test", id = "async-compile-test", category = "Test")]
struct AsyncTestAddin;

impl Addin for AsyncTestAddin {
    type State = State;
    type Error = XllError;
    type Layers = ();

    fn open(_: &OpenContext) -> Result<Opened<Self::State, Self::Layers>, Self::Error> {
        Ok(Opened::new(State, ()))
    }
}

#[derive(ExcelHandleObject)]
struct Dataset {
    size: f64,
}

#[excel_function(name = "TEST.HANDLE.ASYNC")]
async fn async_handle_input(
    #[excel_context(asynchronous)] context: AsyncContext<'_, State>,
    dataset: Handle<'_, Dataset>,
) -> XllResult<f64> {
    let _ = context.state();
    Ok(dataset.size)
}

fn main() {}
