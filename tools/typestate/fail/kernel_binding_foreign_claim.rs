// module: kernel/thread_affine
use super::{BindingEmpty, BindingTxn};
fn invalid(mut txn: BindingTxn<'_, usize, BindingEmpty>, other: BindingTxn<'_, usize, BindingEmpty>) {
    txn.claim = other.claim; // error: E0616
}
