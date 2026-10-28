// module: runtime
use super::open_txn::{Initializing, OpeningTxn};

fn early_commit<A: crate::Addin>(transaction: OpeningTxn<'_, A, Initializing<A>>) {
    let _ = transaction.commit(); // error: E0599
}
