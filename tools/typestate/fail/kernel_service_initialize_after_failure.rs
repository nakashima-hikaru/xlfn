// module: kernel/service_slot
use super::transaction::InitializingTxn;

fn commit_after_failure(transaction: InitializingTxn<'_, (), u32, ()>) {
    let _fault = transaction.fail(());
    let _read = transaction.commit(Box::new(1), |_| {}); // error: E0382
}
