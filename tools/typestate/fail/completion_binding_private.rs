// module: runtime/shutdown/pipeline
use super::super::finalization::CompletionPendingBinding;
use crate::runtime::shutdown::FinalRemoval;
fn invalid<A: crate::Addin>(pending: CompletionPendingBinding<'_, A, FinalRemoval>) {
    let _ = pending.state; // error: E0616
}
