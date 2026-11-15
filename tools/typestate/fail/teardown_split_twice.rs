// module: runtime/shutdown/pipeline
use super::{ModuleAuthorityRecovery, TeardownTxn};

fn invalid<A: crate::Addin, S: ModuleAuthorityRecovery>(
    teardown: TeardownTxn<'_, A, (), S>,
) {
    drop(teardown.split());
    drop(teardown.split()); // error: E0382
}
