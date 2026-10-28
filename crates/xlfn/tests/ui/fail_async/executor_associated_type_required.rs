use xlfn::prelude::*;

struct MissingExecutor;

impl Addin for MissingExecutor {
    xlfn::__private::v1::__xlfn_addin_rtd_types!(@default);
    type SharedState = ();
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(_: &OpenContext<Self>) -> OpenResult<Self> {
        Ok(Opened::new(()))
    }
}

fn main() {}
