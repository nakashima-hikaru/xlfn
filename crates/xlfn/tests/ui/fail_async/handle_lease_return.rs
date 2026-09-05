use xlfn::prelude::*;

struct State;

#[excel_addin(name = "Returned Handle Lease", id = "returned-handle-lease", category = "Test")]
struct ReturnedHandleLeaseAddin;

impl Addin for ReturnedHandleLeaseAddin {
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

#[excel_function(name = "TEST.RETURNED.HANDLE.LEASE")]
async fn returned_handle_lease(dataset: HandleLease<'_, Dataset>) -> HandleLease<'_, Dataset> {
    dataset
}

fn main() {}
