// module: runtime/shutdown/pipeline
use super::{HostDetached, TeardownTxn};

fn invalid<A: crate::Addin>(transaction: TeardownTxn<'_, A, (), HostDetached>) {
    let _ = transaction.seal_services(); // error: E0599
}
