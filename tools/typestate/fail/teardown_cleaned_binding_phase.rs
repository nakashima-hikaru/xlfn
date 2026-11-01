// module: runtime/shutdown/pipeline
use super::{ServicesCleaned, TeardownTxn};
use crate::runtime::capabilities::InstalledLifecycle;
fn invalid<'a, A: crate::Addin>(txn: TeardownTxn<'a, A, (), ServicesCleaned>, installed: Option<InstalledLifecycle<'a, A>>) {
    let (mut owner, _stage) = txn.split();
    owner.binding = installed; // error: E0308
}
