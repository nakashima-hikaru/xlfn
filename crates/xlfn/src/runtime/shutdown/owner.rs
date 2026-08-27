use crate::generation::RemovalAttemptId;
use crate::module_runtime::{ModuleAuthority, ModuleClosing};
use crate::runtime::Runtime;

/// Runtime-side owner of a lifecycle removal claim.
///
/// `RemovalClaim` is issued by the canonical lifecycle state machine. Once a
/// runtime-wide shutdown transaction takes that claim, this owner carries the
/// module close capability until teardown either consumes it or returns a
/// cleanup authority to lifecycle state.
pub(crate) struct RemovalOwner<'runtime, A: crate::Addin> {
    runtime: &'runtime Runtime<A>,
    attempt: RemovalAttemptId,
    module_closing: Option<ModuleClosing>,
    returned_module: Option<Box<ModuleAuthority>>,
}

impl<A: crate::Addin> Drop for RemovalOwner<'_, A> {
    fn drop(&mut self) {
        // An owner may be abandoned before teardown consumes the module
        // capability. Return that capability to the runtime so a waiting
        // removal request can take it over without minting a second close
        // authority.
        let lifecycle = self.runtime.lifecycle_control();
        let mut control = lifecycle.access();
        let closing = self.module_closing.take().map(ModuleAuthority::Closing);
        let returned = match (
            closing,
            self.returned_module.take().map(|authority| *authority),
        ) {
            (Some(_), Some(_)) => xlfn_kernel::invariant::fail_stop(),
            (Some(authority), None) | (None, Some(authority)) => Some(authority),
            (None, None) => None,
        };
        lifecycle.release_removal_claim(&mut control, self.attempt, returned);
        self.runtime.refinement.release_cleanup_owner(self.runtime);
        lifecycle.notify_all();
    }
}

impl<'runtime, A: crate::Addin> RemovalOwner<'runtime, A> {
    pub(crate) fn new(
        runtime: &'runtime Runtime<A>,
        claim: crate::lifecycle::RemovalClaim,
    ) -> Self {
        Self {
            runtime,
            attempt: claim.attempt(),
            module_closing: Some(claim.into_module_closing()),
            returned_module: None,
        }
    }

    pub(crate) fn runtime(&self) -> &'runtime Runtime<A> {
        self.runtime
    }

    pub(crate) fn attempt(&self) -> RemovalAttemptId {
        self.attempt
    }

    pub(crate) fn has_module_closing(&self) -> bool {
        self.module_closing.is_some()
    }

    pub(crate) fn take_module_closing(&mut self) -> ModuleClosing {
        self.module_closing
            .take()
            .expect("removal owner carries module close capability")
    }

    pub(crate) fn return_module_authority(
        &mut self,
        authority: crate::module_runtime::ModuleCleanupAuthority,
    ) {
        if self.module_closing.is_some() || self.returned_module.is_some() {
            xlfn_kernel::invariant::fail_stop();
        }
        self.returned_module = Some(Box::new(authority.into_authority()));
    }
}
