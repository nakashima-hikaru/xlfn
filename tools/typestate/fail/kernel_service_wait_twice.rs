// module: kernel/service_slot
use super::transaction::SealingTxn;

fn drain_twice(transaction: SealingTxn<'_, (), u32, ()>) {
    let _first = transaction.wait();
    let _second = transaction.wait(); // error: E0382
}
