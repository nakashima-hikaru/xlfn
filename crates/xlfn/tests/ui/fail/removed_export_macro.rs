use xlfn::prelude::*;

struct State;

struct OldExportAddin;

impl Addin for OldExportAddin {
    xlfn::__private::v1::__xlfn_addin_rtd_types!(@default);
    type SharedState = State;
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();
    #[cfg(feature = "async")]
    type AsyncExecutor = xlfn::NoAsyncExecutor;

    fn open(_: &OpenContext<Self>) -> Result<Opened<Self::SharedState, Self::LifecycleState, Self::Layers>, Self::Error> {
        Ok(Opened::new(State))
    }
}

xlfn::export!(OldExportAddin);

fn main() {}
