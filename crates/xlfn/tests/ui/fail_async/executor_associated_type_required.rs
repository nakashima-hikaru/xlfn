use xlfn::prelude::*;

struct MissingExecutor;

impl Addin for MissingExecutor {
    type SharedState = ();
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(_: &OpenContext) -> OpenResult<Self> {
        Ok(Opened::new(()))
    }
}

fn main() {}
