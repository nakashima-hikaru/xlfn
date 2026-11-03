// module: runtime/shutdown/pipeline
use super::super::finalization::FinalizationFailure;
use crate::runtime::shutdown::FinalRemoval;
fn invalid<A: crate::Addin>(failure: FinalizationFailure<'_, A, FinalRemoval>) {
    let _ = failure.finish(); // error: E0599
}
