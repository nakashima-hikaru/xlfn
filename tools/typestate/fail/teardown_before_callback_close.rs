// module: runtime/shutdown/pipeline
use super::{ProducersStopped, TeardownTxn};

fn invalid<A: crate::Addin>(transaction: TeardownTxn<'_, A, (), ProducersStopped>) {
    let _ = transaction.detach_host(); // error: E0599
}
