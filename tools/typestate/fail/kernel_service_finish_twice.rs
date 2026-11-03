// module: kernel/service_slot
use super::transaction::QuiescedTxn;

fn finish_twice(transaction: QuiescedTxn<'_, (), u32, ()>) {
    let _first = transaction.finish(|_| Ok(()));
    let _second = transaction.finish(|_| Ok(())); // error: E0382
}
