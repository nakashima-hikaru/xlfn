use xlfn::prelude::*;

struct State;

struct OldExportAddin;

impl Addin for OldExportAddin {
    type State = State;
    type Error = XllError;

    fn open(_: &OpenContext) -> Result<Self::State, Self::Error> {
        Ok(State)
    }
}

xlfn::export!(OldExportAddin);

fn main() {}
