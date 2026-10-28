// module: runtime
use super::open_txn::{HostMutated, OpeningTxn};

fn double_commit<A: crate::Addin>(transaction: OpeningTxn<'_, A, HostMutated<A>>) {
    let _ = transaction.commit();
    let _ = transaction.commit(); // error: E0382
}
