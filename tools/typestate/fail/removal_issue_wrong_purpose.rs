// module: runtime
use crate::lifecycle::{FinalCloseDecision, IssuedRemovalClaim, LifecycleAccess};
use super::shutdown::OpenRollback;

fn invalid<'issuer, A: crate::Addin>(access: &mut LifecycleAccess<'issuer, A>) -> Option<IssuedRemovalClaim<'issuer, A, OpenRollback>> {
    match access.final_close_decision() {
        FinalCloseDecision::Closing(closing) => closing.try_claim(), // error: E0308
        _ => None,
    }
}
