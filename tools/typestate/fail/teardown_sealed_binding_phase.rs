// module: runtime/shutdown/pipeline
use super::{ServicesSealed, TeardownTxn};
use crate::runtime::capabilities::EmptyLifecycle;
fn invalid<'a, A: crate::Addin>(txn: TeardownTxn<'a, A, (), ServicesSealed<'a, A>>, empty: EmptyLifecycle<'a, A>) {
    let (mut owner, _stage) = txn.split();
    owner.binding = empty; // error: E0308
}
