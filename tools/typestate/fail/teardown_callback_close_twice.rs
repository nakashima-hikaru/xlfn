// module: runtime/shutdown/pipeline
use super::{ProducersStopped, TeardownTxn};

fn invalid<A: crate::Addin>(transaction: TeardownTxn<'_, A, (), ProducersStopped>) {
    let transaction = transaction.close_module_callbacks();
    let _ = transaction.close_module_callbacks(); // error: E0599
}
