// module: kernel/thread_affine
use super::{BindingEmpty, BindingOccupied, BindingTxn};
fn empty(txn: BindingTxn<'_, usize, BindingEmpty>) {
    let occupied = txn.install(7).unwrap();
    let (_value, empty) = occupied.take().unwrap();
    empty.release().unwrap();
}
fn occupied(txn: BindingTxn<'_, usize, BindingOccupied>) {
    let (_value, empty) = txn.take().unwrap();
    empty.release().unwrap();
}
