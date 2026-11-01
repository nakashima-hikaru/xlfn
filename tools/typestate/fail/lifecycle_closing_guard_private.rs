// module: runtime
use crate::lifecycle::FinalCloseDecision;

fn invalid<A: crate::Addin>(decision: FinalCloseDecision<'_, '_, A>) {
    if let FinalCloseDecision::Closing(closing) = decision {
        let _guard = closing.access; // error: E0616
    }
}
