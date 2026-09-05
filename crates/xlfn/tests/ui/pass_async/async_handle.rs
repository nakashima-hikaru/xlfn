use xlfn::prelude::*;

struct State;

#[excel_addin(name = "Async Handle Compile Test", id = "async-handle-compile-test", category = "Test")]
struct AsyncHandleAddin;

impl Addin for AsyncHandleAddin {
    type SharedState = State;
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(_: &OpenContext) -> Result<Opened<Self::SharedState, Self::LifecycleState, Self::Layers>, Self::Error> {
        Ok(Opened::new(State, (), ()))
    }
}

#[derive(ExcelHandleObject)]
struct Dataset {
    size: f64,
}

fn assert_send_sync<T: Send + Sync>() {}

#[excel_function(name = "TEST.HANDLE.ASYNC")]
async fn async_handle(
    #[excel_context(asynchronous)] context: AsyncContext<'_, AsyncHandleAddin>,
    dataset: HandleLease<'_, Dataset>,
    time: f64,
) -> XllResult<f64> {
    let _ = context.state();
    std::future::ready(()).await;
    Ok(dataset.size + time)
}

#[excel_function(name = "TEST.HANDLE.ASYNC.NO_CONTEXT")]
async fn async_handle_without_context(dataset: HandleLease<'_, Dataset>, time: f64) -> f64 {
    std::future::ready(()).await;
    dataset.size + time
}

fn main() {
    assert_send_sync::<HandleLease<'_, Dataset>>();
}
