// module: runtime/shutdown/pipeline
use super::super::finalization::FinalizationFailure;
use super::super::FinalRemoval;

fn retry<A: crate::Addin>(failure: FinalizationFailure<'_, A, FinalRemoval>) {
    let _ = failure.recovery; // error: E0616
}
