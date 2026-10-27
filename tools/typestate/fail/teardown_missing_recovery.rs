// module: runtime/shutdown/pipeline
use super::TeardownTxn;

fn invalid<A: crate::Addin>(transaction: TeardownTxn<'_, A, (), ()>) { // error: E0277
    drop(transaction); // error: E0277
}
