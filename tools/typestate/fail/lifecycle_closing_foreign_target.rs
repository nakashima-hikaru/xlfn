// module: runtime
use crate::lifecycle::{FinalCloseDecision, LifecycleCoordinator};

fn invalid<A: crate::Addin>(decision: FinalCloseDecision<'_, '_, A>, other: &LifecycleCoordinator<A>) {
    if let FinalCloseDecision::Closing(closing) = decision {
        let _ = closing.try_claim(other); // error: E0061
    }
}
