// module: kernel/service_slot
use super::transaction::InitializingTxn;

fn initialize_twice(transaction: InitializingTxn<'_, (), u32, ()>) {
    let _first = transaction.commit(Box::new(1), |_| {});
    let _second = transaction.commit(Box::new(2), |_| {}); // error: E0382
}
