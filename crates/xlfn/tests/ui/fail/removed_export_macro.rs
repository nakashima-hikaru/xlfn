use xlfn::prelude::*;

struct State;

struct OldExportAddin;

impl Addin for OldExportAddin {
    type State = State;
    type Error = XllError;
    type Layers = ();

    fn open(_: &OpenContext) -> Result<Opened<Self::State, Self::Layers>, Self::Error> {
        Ok(Opened::new(State, ()))
    }
}

xlfn::export!(OldExportAddin);

fn main() {}
