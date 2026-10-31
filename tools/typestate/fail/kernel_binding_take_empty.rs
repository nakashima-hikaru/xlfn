// module: kernel/thread_affine
use super::{BindingEmpty, BindingTxn};
fn invalid(txn: BindingTxn<'_, usize, BindingEmpty>) {
    txn.take(); // error: E0599
}
