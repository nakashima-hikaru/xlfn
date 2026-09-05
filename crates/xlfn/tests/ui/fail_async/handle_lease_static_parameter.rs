use xlfn::prelude::*;

struct State;

#[excel_addin(name = "Static Handle Lease", id = "static-handle-lease", category = "Test")]
struct StaticHandleLeaseAddin;

impl Addin for StaticHandleLeaseAddin {
    type SharedState = State;
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(_: &OpenContext) -> Result<Opened<Self::SharedState, Self::LifecycleState, Self::Layers>, Self::Error> {
        Ok(Opened::new(State, (), ()))
    }
}

#[derive(ExcelHandleObject)]
struct Dataset;

#[excel_function(name = "TEST.STATIC.HANDLE.LEASE")]
async fn static_handle_lease(dataset: HandleLease<'static, Dataset>) -> f64 {
    let _ = dataset;
    0.0
}

fn main() {}
