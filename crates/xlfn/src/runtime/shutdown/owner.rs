use crate::generation::RemovalAttemptId;
use crate::module_runtime::{ModuleCleanupAuthority, ModuleClosing};
use crate::runtime::capabilities::ShutdownDeps;

/// The initial removal claim and its unique, undrained module capability.
/// Draining consumes this value, so a caller cannot drain the same claim twice.
pub(crate) struct RemovalOwner<'runtime, A: crate::Addin, K> {
    deps: ShutdownDeps<'runtime, A>,
    state: Option<(RemovalClaimGuard<'runtime, A>, ModuleClosing)>,
    _kind: std::marker::PhantomData<K>,
}

/// Keeps cleanup ownership until the terminal callback has returned. The
/// pipeline owns module authority separately and returns it here on failure.
pub(crate) struct RemovalClaimGuard<'runtime, A: crate::Addin> {
    lifecycle: &'runtime crate::lifecycle::LifecycleCoordinator<A>,
    observer: &'runtime crate::runtime::observer::RuntimeObserver,
    attempt: RemovalAttemptId,
    returned: Option<Box<ModuleCleanupAuthority>>,
}

impl<A: crate::Addin> Drop for RemovalClaimGuard<'_, A> {
    fn drop(&mut self) {
        let lifecycle = crate::lifecycle::LifecycleControl::new(self.lifecycle);
        let mut control = lifecycle.access();
        let returned = self
            .returned
            .take()
            .map(|authority| authority.into_authority());
        lifecycle.release_removal_claim(&mut control, self.attempt, returned);
        self.observer.release_cleanup_owner();
        lifecycle.notify_all();
    }
}

impl<'runtime, A: crate::Addin, K> RemovalOwner<'runtime, A, K> {
    pub(in crate::runtime) fn from_claim(
        deps: ShutdownDeps<'runtime, A>,
        claim: crate::runtime::orchestration::RemovalPurposeClaim<K>,
    ) -> Self {
        let claim = claim.into_claim();
        let guard = RemovalClaimGuard {
            lifecycle: deps.lifecycle(),
            observer: deps.observer(),
            attempt: claim.attempt(),
            returned: None,
        };
        Self {
            deps,
            state: Some((guard, claim.into_module_closing())),
            _kind: std::marker::PhantomData,
        }
    }

    pub(super) fn into_parts(
        mut self,
    ) -> (
        ShutdownDeps<'runtime, A>,
        RemovalClaimGuard<'runtime, A>,
        ModuleClosing,
    ) {
        let (guard, closing) = self.state.take().expect("removal owner consumed once");
        (self.deps, guard, closing)
    }

    #[cfg(test)]
    pub(crate) fn into_test_parts(self) -> (RemovalClaimGuard<'runtime, A>, ModuleClosing) {
        let (_, guard, closing) = self.into_parts();
        (guard, closing)
    }
}

impl<A: crate::Addin, K> Drop for RemovalOwner<'_, A, K> {
    fn drop(&mut self) {
        if let Some((mut guard, closing)) = self.state.take() {
            guard.return_module_authority(ModuleCleanupAuthority::Closing(closing));
        }
    }
}

impl<A: crate::Addin> RemovalClaimGuard<'_, A> {
    pub(crate) fn attempt(&self) -> RemovalAttemptId {
        self.attempt
    }

    pub(super) fn return_module_authority(&mut self, authority: ModuleCleanupAuthority) {
        if self.returned.replace(Box::new(authority)).is_some() {
            xlfn_kernel::invariant::fail_stop();
        }
    }
}
