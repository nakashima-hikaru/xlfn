// module: runtime/shutdown/pipeline
use super::{CallbacksClosed, TeardownTxn};
use crate::runtime::shutdown::FinalRemoval;

fn invalid<A: crate::Addin>(
    transaction: TeardownTxn<'_, A, FinalRemoval, CallbacksClosed>,
    runtime: &crate::runtime::Runtime<A>,
    lifecycle: &crate::runtime::AddinLifecycleAccess<'_, A>,
) {
    let _ = transaction.quiesce_addin(runtime, lifecycle); // error: E0599
}
