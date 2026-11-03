// module: kernel/service_slot/transaction
use super::QuiescedTxn;
fn forge(source: QuiescedTxn<'_, (), u32, ()>) {
    let _drained = QuiescedTxn { ..source }; // error: E0451
}
