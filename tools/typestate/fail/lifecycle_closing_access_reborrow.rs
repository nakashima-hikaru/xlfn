// module: runtime
use crate::lifecycle::{FinalCloseDecision, LifecycleAccess};

fn invalid<A: crate::Addin>(access: &mut LifecycleAccess<'_, A>) {
    if let FinalCloseDecision::Closing(closing) = access.final_close_decision() {
        let _ = access.phase(); // error: E0502
        let _ = closing.try_claim();
    }
}
