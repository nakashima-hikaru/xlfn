// module: kernel/service_slot
use super::transaction::SealingTxn;

fn finish(transaction: SealingTxn<'_, (), u32, ()>) {
    let _sealed = transaction.finish(|_| Ok::<(), ()>(())); // error: E0599
}
