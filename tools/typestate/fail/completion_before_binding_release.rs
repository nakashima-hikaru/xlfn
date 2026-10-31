// module: runtime/shutdown/pipeline
use super::super::finalization::CompletionPendingBinding;
use crate::runtime::shutdown::FinalRemoval;
use crate::runtime::transactions::RemovalSuccess;
fn invalid<A: crate::Addin>(pending: CompletionPendingBinding<'_, A, FinalRemoval>) {
    let _ = RemovalSuccess::Closed { finalized: pending }; // error: E0308
}
