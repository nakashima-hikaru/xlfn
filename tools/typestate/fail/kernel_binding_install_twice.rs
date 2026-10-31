// module: kernel/thread_affine
use super::{BindingEmpty, BindingTxn};
fn invalid(txn: BindingTxn<'_, usize, BindingEmpty>) {
    let _occupied = txn.install(1);
    let _second = txn.install(2); // error: E0382
}
