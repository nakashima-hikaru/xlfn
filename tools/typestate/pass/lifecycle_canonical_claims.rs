// module: runtime
use crate::lifecycle::{FinalCloseDecision, IssuedRemovalClaim, LifecycleAccess, RollbackDecision};
use super::shutdown::{FinalRemoval, OpenRollback};

fn final_claim<'issuer, A: crate::Addin>(access: &mut LifecycleAccess<'issuer, A>) -> Option<IssuedRemovalClaim<'issuer, A, FinalRemoval>> {
    access.begin_final_removal_request();
    match access.final_close_decision() {
        FinalCloseDecision::Closing(closing) => closing.try_claim(),
        FinalCloseDecision::ClosedIdle | FinalCloseDecision::Quarantined | FinalCloseDecision::Wait => None,
    }
}

fn rollback_claim<'issuer, A: crate::Addin>(access: &mut LifecycleAccess<'issuer, A>) -> Option<IssuedRemovalClaim<'issuer, A, OpenRollback>> {
    match access.rollback_decision() {
        RollbackDecision::Claim(claim) => Some(claim),
        RollbackDecision::ClosedIdle | RollbackDecision::ClosingOwnsCleanup | RollbackDecision::Unavailable | RollbackDecision::Wait => None,
    }
}

fn wait_for_original_coordinator<A: crate::Addin>(access: &mut LifecycleAccess<'_, A>) {
    access.wait_for_change();
}
