// module: handle
use super::registry::{HandleRegistry, HandleRegistrySealed};

fn invalid<'registry>() -> HandleRegistrySealed<'registry> {
    let registry = HandleRegistry::try_new(1).unwrap();
    registry.seal().unwrap() // error: E0515
}
