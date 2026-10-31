// module: kernel/service_slot
use super::transaction::QuiescedTxn;

fn substitute_recovery<'a>(mut transaction: QuiescedTxn<'a, (), u32, ()>, other: QuiescedTxn<'a, (), u32, ()>) {
    transaction.recovery = other.recovery; // error: E0616
}
