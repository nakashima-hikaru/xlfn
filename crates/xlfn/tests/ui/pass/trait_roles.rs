use xlfn::prelude::*;

pub struct State;

#[excel_addin(name = "Compile Test", id = "compile-test", category = "Test")]
pub struct TestAddin;

impl Addin for TestAddin {
    type State = State;
    type Error = XllError;

    fn open(_: &OpenContext) -> Result<Self::State, Self::Error> {
        Ok(State)
    }
}

#[derive(ExcelHandleObject)]
pub struct Dataset;

type DatasetHandle = Handle<Dataset>;
type DatasetObject = Dataset;
type FunctionResult<T> = XllResult<T>;
type MainContext<'state, 'scope> = MainThreadContext<'state, 'scope, State>;

mod reexported {
    pub use xlfn::context::ThreadSafeContext as WorkerContext;
    pub use xlfn::handle::Handle;
}

use xlfn::handle::Handle as ObjectHandle;

#[excel_function(name = "TEST.HANDLE.DIRECT")]
fn direct_handle() -> Dataset {
    Dataset
}

#[excel_function(name = "TEST.HANDLE.OBJECT.ALIAS")]
fn aliased_object_handle() -> DatasetObject {
    Dataset
}

#[excel_function(name = "TEST.HANDLE.RESULT")]
fn result_handle() -> XllResult<Dataset> {
    Ok(Dataset)
}

#[excel_function(name = "TEST.HANDLE.RESULT.ALIASES")]
fn aliased_result_handle() -> FunctionResult<DatasetObject> {
    Ok(Dataset)
}

#[excel_function(name = "TEST.HANDLE.REEXPORT")]
fn reexport_handle(value: reexported::Handle<Dataset>) -> reexported::Handle<Dataset> {
    value
}

#[excel_function(name = "TEST.HANDLE.RENAME")]
fn renamed_handle(value: ObjectHandle<Dataset>) -> ObjectHandle<Dataset> {
    value
}

#[excel_function(name = "TEST.HANDLE.CONSUME")]
fn consume_handle(value: DatasetHandle) -> f64 {
    let _ = &*value;
    1.0
}

#[excel_function(name = "TEST.HANDLE.OPTION")]
fn optional_handle(value: Option<DatasetHandle>) -> f64 {
    f64::from(u8::from(value.is_some()))
}

#[excel_function(name = "TEST.CONTEXT.ALIAS")]
fn alias_context(#[excel_context(main_thread)] context: MainContext<'_, '_>) -> f64 {
    let _ = context.state();
    1.0
}

#[excel_function(name = "TEST.CONTEXT.REEXPORT", thread_safe)]
fn reexported_context(
    #[excel_context(thread_safe)] context: reexported::WorkerContext<'_, State>,
) -> f64 {
    let _ = context.state();
    1.0
}

#[excel_function(name = "TEST.VALUE", thread_safe)]
fn ordinary_value(value: f64) -> f64 {
    value
}

#[excel_function(thread_safe)]
fn ordinary_result(value: f64) -> Result<f64, XllError> {
    Ok(value)
}

fn main() {}
