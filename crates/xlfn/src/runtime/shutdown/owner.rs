use crate::generation::RemovalAttemptId;
use crate::module_runtime::{ModuleAuthority, ModuleCleanupAuthority, ModuleClosing};
use crate::runtime::capabilities::ShutdownDeps;

/// Runtime-side owner of a lifecycle removal claim.
///
/// `RemovalClaim` is issued by the canonical lifecycle state machine. Once a
/// runtime-wide shutdown transaction takes that claim, this owner carries the
/// module close capability until teardown either consumes it or returns a
/// cleanup authority to lifecycle state.
pub(crate) struct RemovalOwner<'runtime, A: crate::Addin> {
    lifecycle: &'runtime crate::lifecycle::LifecycleCoordinator<A>,
    observer: &'runtime crate::runtime::observer::RuntimeObserver,
    attempt: RemovalAttemptId,
    authority: RemovalAuthority,
}

/// The claim holds either the initial close capability, no capability while
/// the teardown pipeline owns it, or the capability returned for a takeover.
/// Initial and returned authority cannot coexist.
enum RemovalAuthority {
    Closing(ModuleClosing),
    InPipeline,
    Returned(Box<ModuleCleanupAuthority>),
}

impl<A: crate::Addin> Drop for RemovalOwner<'_, A> {
    fn drop(&mut self) {
        // An owner may be abandoned before teardown consumes the module
        // capability. Return that capability to the runtime so a waiting
        // removal request can take it over without minting a second close
        // authority.
        let lifecycle = crate::lifecycle::LifecycleControl::new(self.lifecycle);
        let mut control = lifecycle.access();
        let returned = match std::mem::replace(&mut self.authority, RemovalAuthority::InPipeline) {
            RemovalAuthority::Closing(closing) => Some(ModuleAuthority::Closing(closing)),
            RemovalAuthority::Returned(authority) => Some(authority.into_authority()),
            RemovalAuthority::InPipeline => None,
        };
        lifecycle.release_removal_claim(&mut control, self.attempt, returned);
        self.observer.release_cleanup_owner();
        lifecycle.notify_all();
    }
}

impl<'runtime, A: crate::Addin> RemovalOwner<'runtime, A> {
    pub(crate) fn new(
        deps: ShutdownDeps<'runtime, A>,
        claim: crate::lifecycle::RemovalClaim,
    ) -> Self {
        Self {
            lifecycle: deps.lifecycle(),
            observer: deps.observer(),
            attempt: claim.attempt(),
            authority: RemovalAuthority::Closing(claim.into_module_closing()),
        }
    }

    pub(crate) fn attempt(&self) -> RemovalAttemptId {
        self.attempt
    }

    pub(crate) fn has_module_closing(&self) -> bool {
        matches!(self.authority, RemovalAuthority::Closing(_))
    }

    pub(crate) fn take_module_closing(&mut self) -> ModuleClosing {
        if !self.has_module_closing() {
            xlfn_kernel::invariant::fail_stop();
        }
        let RemovalAuthority::Closing(closing) =
            std::mem::replace(&mut self.authority, RemovalAuthority::InPipeline)
        else {
            unreachable!("removal owner carries module close capability");
        };
        closing
    }

    pub(crate) fn return_module_authority(&mut self, authority: ModuleCleanupAuthority) {
        if !matches!(self.authority, RemovalAuthority::InPipeline) {
            xlfn_kernel::invariant::fail_stop();
        }
        self.authority = RemovalAuthority::Returned(Box::new(authority));
    }
}
