// module: kernel/thread_affine
use super::{BindingOccupied, BindingTxn};
fn invalid(txn: BindingTxn<'_, usize, BindingOccupied>) {
    txn.release(); // error: E0599
}
