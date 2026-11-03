use xlfn::execution::{CallMetadata, CallOutcome, UdfLayer, UdfLayerGuard, UdfLayers};
use xlfn::XllResult;

struct Layer;
struct Guard;

impl UdfLayer for Layer {
    type Guard = Guard;

    fn enter(&self, _: &CallMetadata) -> XllResult<Self::Guard> {
        Ok(Guard)
    }
}

impl UdfLayerGuard for Guard {
    fn exit(self, _: &CallOutcome<'_>) {}
}

fn require_layers<L: UdfLayers>() {}

fn main() {
    require_layers::<(Layer,)>();
}
