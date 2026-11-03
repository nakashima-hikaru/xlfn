// module: cache
use super::{CreatorPinGuard, NodePtr};

fn invalid(observation: NodePtr<u32>) {
    let _ = CreatorPinGuard::new(observation.0); // error: E0308
}
