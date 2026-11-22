// module: runtime/shutdown/pipeline
use super::{BindingPhase, ServicesSealed, TeardownTxn};

fn typed_drop<A: crate::Addin, S: BindingPhase>(
    transaction: TeardownTxn<'_, A, (), S>,
) {
    drop(transaction);
}

fn borrowed_stage<'runtime, A: crate::Addin>(
    transaction: TeardownTxn<'runtime, A, (), ServicesSealed<'runtime, A>>,
) {
    drop(transaction);
}
