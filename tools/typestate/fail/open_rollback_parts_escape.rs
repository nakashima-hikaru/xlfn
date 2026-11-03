// module: runtime
use super::open_txn::{HostAttached, OpeningTxn};

fn invalid<A: crate::Addin>(opening: OpeningTxn<'_, A, HostAttached<A>>) {
    let _ = opening.into_rollback_parts(); // error: E0599
}
