use xlfn::prelude::*;

struct State;

struct OldExportAddin;

impl Addin for OldExportAddin {
    type SharedState = State;
    type LifecycleState = ();
    type Error = XllError;
    type Layers = ();

    fn open(_: &OpenContext) -> Result<Opened<Self::SharedState, Self::LifecycleState, Self::Layers>, Self::Error> {
        Ok(Opened::new(State, (), ()))
    }
}

xlfn::export!(OldExportAddin);

fn main() {}
