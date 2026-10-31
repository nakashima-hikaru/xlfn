// module: runtime/shutdown/pipeline
use super::{ServicesCleaned, TeardownTxn};
use crate::runtime::shutdown::FinalRemoval;
fn invalid<A: crate::Addin>(txn: TeardownTxn<'_, A, FinalRemoval, ServicesCleaned>) {
    let _ = txn.empty_binding; // error: E0609
}
