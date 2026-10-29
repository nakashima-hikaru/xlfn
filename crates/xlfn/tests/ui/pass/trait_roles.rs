use xlfn::prelude::*;

pub struct State;

pub struct TestAddin;

#[excel_addin(name = "Compile Test", id = "compile-test", category = "Test")]
impl Addin for TestAddin {
    type SharedState = State;
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(
        _: &OpenContext<Self>,
    ) -> Result<Opened<Self::SharedState, Self::LifecycleState, Self::Layers>, Self::Error> {
        Ok(Opened::new(State))
    }
}

#[derive(ExcelHandleObject)]
pub struct Dataset;

#[derive(ExcelHandleObject)]
pub struct GenericDataset<T>
where
    T: std::fmt::Debug,
{
    value: T,
}

type DatasetHandle<'call> = Handle<'call, Dataset>;
type DatasetObject = Dataset;
type FunctionResult<T> = XllResult<T>;

mod reexported {
    pub use xlfn::handle::{Handle, HandleAlias};
}

use xlfn::handle::{Handle as ObjectHandle, HandleAlias as ObjectHandleAlias};

#[excel_function(name = "TEST.HANDLE.DIRECT", handle_reuse = inputs)]
fn direct_handle() -> Dataset {
    Dataset
}

#[excel_function(name = "TEST.HANDLE.GENERIC")]
fn generic_handle() -> GenericDataset<i32> {
    GenericDataset { value: 42 }
}

#[excel_function(name = "TEST.HANDLE.OBJECT.ALIAS")]
fn aliased_object_handle() -> DatasetObject {
    Dataset
}

#[excel_function(name = "TEST.HANDLE.RESULT")]
fn result_handle() -> XllResult<Dataset> {
    Ok(Dataset)
}

#[excel_function(name = "TEST.HANDLE.RESULT.ALIASES", handle_reuse = never)]
fn aliased_result_handle() -> FunctionResult<DatasetObject> {
    Ok(Dataset)
}

#[excel_function(name = "TEST.HANDLE.REEXPORT")]
fn reexport_handle(value: reexported::Handle<'_, Dataset>) -> reexported::HandleAlias<'_, Dataset> {
    value.alias()
}

#[excel_function(name = "TEST.HANDLE.RENAME")]
fn renamed_handle(value: ObjectHandle<'_, Dataset>) -> ObjectHandleAlias<'_, Dataset> {
    value.alias()
}

#[excel_function(name = "TEST.HANDLE.CONSUME")]
fn consume_handle(value: DatasetHandle<'_>) -> f64 {
    let _: &Dataset = value.as_ref();
    let _: xlfn::handle::HandleObjectId = value.object_id();
    1.0
}

#[excel_function(name = "TEST.HANDLE.OPTION")]
fn optional_handle(value: Option<DatasetHandle<'_>>) -> f64 {
    f64::from(u8::from(value.is_some()))
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
