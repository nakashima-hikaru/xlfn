// module: runtime
use crate::lifecycle::FinalCloseDecision;

fn invalid<A: crate::Addin>(decision: FinalCloseDecision<'_, '_, A>) {
    if let FinalCloseDecision::Closing(closing) = decision {
        let _ = closing.try_claim();
        let _ = closing.try_claim(); // error: E0382
    }
}
