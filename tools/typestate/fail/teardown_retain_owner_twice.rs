// module: runtime/shutdown/pipeline
use super::{ExecutionDrained, TeardownTxn};

fn invalid<A: crate::Addin>(teardown: TeardownTxn<'_, A, (), ExecutionDrained>) {
    let (owner, _) = teardown.split();
    drop(owner.retain());
    drop(owner.retain()); // error: E0382
}
