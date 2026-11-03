// module: kernel/service_slot
use super::transaction::{QuiescedTxn, SealingTxn};

fn forge_drained(transaction: SealingTxn<'_, (), u32, ()>) {
    let _forged = QuiescedTxn { recovery: transaction.recovery }; // error: E0616
}
