//! Canonical removal decisions and their issuer-bound acquisition rights.
use super::*;
use crate::runtime::{FinalRemoval, OpenRollback};

/// A claim's purpose, issuer and module authority are issued atomically.
/// Ordinary abandonment retains the canonical claim; it cannot advance cleanup.
pub(crate) struct IssuedRemovalClaim<'issuer, A: crate::Addin, K> {
    issuer: &'issuer LifecycleCoordinator<A>,
    claim: RemovalClaim,
    kind: std::marker::PhantomData<fn() -> K>,
}

impl<'issuer, A: crate::Addin, K> IssuedRemovalClaim<'issuer, A, K> {
    fn issue(issuer: &'issuer LifecycleCoordinator<A>, claim: RemovalClaim) -> Self {
        Self {
            issuer,
            claim,
            kind: std::marker::PhantomData,
        }
    }

    /// Consumes the claim without selecting a different recipient. The
    /// issuer travels with the attempt and module authority.
    pub(crate) fn into_issuer_parts(
        self,
    ) -> (
        &'issuer LifecycleCoordinator<A>,
        RemovalAttemptId,
        crate::module_runtime::ModuleClosing,
    ) {
        (
            self.issuer,
            self.claim.attempt(),
            self.claim.into_module_closing(),
        )
    }

    #[cfg(test)]
    pub(crate) fn into_parts(
        self,
        expected: &LifecycleCoordinator<A>,
    ) -> Result<(RemovalAttemptId, crate::module_runtime::ModuleClosing), Self> {
        if !std::ptr::eq(self.issuer, expected) {
            return Err(self);
        }
        Ok((self.claim.attempt(), self.claim.into_module_closing()))
    }
}

pub(crate) enum FinalCloseDecision<'access, 'issuer, A: crate::Addin> {
    ClosedIdle,
    Quarantined,
    Wait,
    Closing(FinalClosing<'access, 'issuer, A>),
}

/// The canonical closing state remains locked until its request is observed.
pub(crate) struct FinalClosing<'access, 'issuer, A: crate::Addin> {
    access: &'access mut LifecycleAccess<'issuer, A>,
}

impl<'access, 'issuer, A: crate::Addin> FinalClosing<'access, 'issuer, A> {
    #[cfg(test)]
    pub(crate) fn belongs_to(&self, coordinator: &LifecycleCoordinator<A>) -> bool {
        std::ptr::eq(self.access.coordinator, coordinator)
    }

    pub(crate) fn try_claim(self) -> Option<IssuedRemovalClaim<'issuer, A, FinalRemoval>> {
        let access = self.access;
        let issuer = access.coordinator;
        issuer
            .claim_removal(access)
            .map(|claim| IssuedRemovalClaim::issue(issuer, claim))
    }
}

pub(crate) enum RollbackDecision<'issuer, A: crate::Addin> {
    ClosedIdle,
    ClosingOwnsCleanup,
    Unavailable,
    Wait,
    Claim(IssuedRemovalClaim<'issuer, A, OpenRollback>),
}

impl<'issuer, A: crate::Addin> LifecycleAccess<'issuer, A> {
    /// Waits on the same coordinator that issued this locked access.
    pub(crate) fn wait_for_change(&mut self) {
        self.coordinator.wait(self);
    }

    /// Invalidates sampled opening epochs even when this removal is a no-op.
    pub(crate) fn begin_final_removal_request(&mut self) {
        self.coordinator.begin_removal_request(self);
    }

    /// Return admission must already be closed before this publishes Closing.
    pub(crate) fn final_close_decision(&mut self) -> FinalCloseDecision<'_, 'issuer, A> {
        match self.coordinator.request_closing(self) {
            CloseRequestDisposition::ClosedIdle => FinalCloseDecision::ClosedIdle,
            CloseRequestDisposition::Finalizing => FinalCloseDecision::Wait,
            CloseRequestDisposition::Closing => {
                FinalCloseDecision::Closing(FinalClosing { access: self })
            }
            CloseRequestDisposition::Quarantined => FinalCloseDecision::Quarantined,
        }
    }

    pub(crate) fn rollback_decision(&mut self) -> RollbackDecision<'issuer, A> {
        match &self.core.state {
            LifecycleState::Closed(ClosedState::Idle) => RollbackDecision::ClosedIdle,
            LifecycleState::Closed(ClosedState::Finalizing { .. }) | LifecycleState::Closing(_) => {
                RollbackDecision::ClosingOwnsCleanup
            }
            LifecycleState::Opening { .. }
            | LifecycleState::Open { .. }
            | LifecycleState::Quarantined(_) => RollbackDecision::Unavailable,
            LifecycleState::OpenRollbackPending(_) => {
                let issuer = self.coordinator;
                match issuer.claim_removal(self) {
                    Some(claim) => {
                        RollbackDecision::Claim(IssuedRemovalClaim::issue(issuer, claim))
                    }
                    None => RollbackDecision::Wait,
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn request_closing_for_test(&mut self) {
        self.coordinator.request_closing(self);
    }
}
