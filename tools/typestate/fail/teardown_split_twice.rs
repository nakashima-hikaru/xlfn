// module: runtime/shutdown/pipeline
use super::{BindingPhase, TeardownTxn};

fn invalid<A: crate::Addin, S: BindingPhase>(
    teardown: TeardownTxn<'_, A, (), S>,
) {
    drop(teardown.split());
    drop(teardown.split()); // error: E0382
}
