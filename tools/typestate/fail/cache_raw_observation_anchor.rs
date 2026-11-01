// module: cache
use super::{NodePtr, ResidentPin};

fn invalid(observation: NodePtr<u32>) {
    let _ = ResidentPin::acquire(observation); // error: E0308
}
