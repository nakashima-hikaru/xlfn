// module: runtime
use super::open_txn::{GenerationStaged, OpeningTxn};

fn early_commit<A: crate::Addin>(transaction: OpeningTxn<'_, A, GenerationStaged<A>>) {
    let _ = transaction.commit(); // error: E0599
}
