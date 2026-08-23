use parking_lot::{Condvar, Mutex};
use std::num::NonZeroU32;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct ComModuleState {
    pub(super) live_factories: usize,
    pub(super) live_servers: usize,
    pub(super) server_locks: usize,
    pub(super) in_flight_calls: usize,
    pub(super) outstanding_git_cookies: usize,
    pub(super) revocation_debt: usize,
}

impl ComModuleState {
    pub(super) fn is_quiescent(self) -> bool {
        self.live_factories == 0
            && self.live_servers == 0
            && self.server_locks == 0
            && self.in_flight_calls == 0
            && self.outstanding_git_cookies == 0
            && self.revocation_debt == 0
    }

    fn has_only_git_blockers(self) -> bool {
        self.live_factories == 0
            && self.live_servers == 0
            && self.server_locks == 0
            && self.in_flight_calls == 0
            && (self.outstanding_git_cookies != 0 || self.revocation_debt != 0)
    }
}

struct ComModuleLifetimeInner {
    state: ComModuleState,
    /// Cookies whose GIT revocation failed. The corresponding
    /// `state.revocation_debt` count also includes claims temporarily removed
    /// from this queue while a retry is in flight.
    git_revocation_debt: Vec<NonZeroU32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ComModuleQuiescenceError {
    pub(super) state: ComModuleState,
}

pub(super) struct GitRevocationDebtClaim {
    cookie: Option<NonZeroU32>,
}

impl GitRevocationDebtClaim {
    pub(super) fn raw(&self) -> u32 {
        self.cookie
            .expect("unresolved GIT revocation debt contains a cookie")
            .get()
    }

    pub(super) fn resolve(mut self) {
        let _cookie = self
            .cookie
            .take()
            .expect("GIT revocation debt is resolved once");
        super::module_lifetime().git_revocation_debt_resolved();
    }
}

impl Drop for GitRevocationDebtClaim {
    fn drop(&mut self) {
        let Some(cookie) = self.cookie.take() else {
            return;
        };

        super::module_lifetime().requeue_git_revocation_debt(cookie);
    }
}

pub(crate) struct ComModuleLifetime {
    inner: Mutex<ComModuleLifetimeInner>,
    quiescent: Condvar,
    #[cfg(any(test, feature = "refinement"))]
    pub(super) ghost: Mutex<Option<crate::shutdown_refinement::GhostHandle>>,
}

impl ComModuleLifetime {
    pub(crate) const fn new() -> Self {
        Self {
            inner: Mutex::new(ComModuleLifetimeInner {
                state: ComModuleState {
                    live_factories: 0,
                    live_servers: 0,
                    server_locks: 0,
                    in_flight_calls: 0,
                    outstanding_git_cookies: 0,
                    revocation_debt: 0,
                },
                git_revocation_debt: Vec::new(),
            }),
            quiescent: Condvar::new(),
            #[cfg(any(test, feature = "refinement"))]
            ghost: Mutex::new(None),
        }
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(super) fn set_ghost(&self, ghost: crate::shutdown_refinement::GhostHandle) {
        *self.ghost.lock() = Some(ghost);
    }

    #[cfg(any(test, feature = "refinement"))]
    fn record_ghost_event(&self, event: crate::shutdown_refinement::GhostEvent) {
        if let Some(ghost) = self.ghost.lock().as_ref().cloned() {
            ghost.record_event(event);
        }
    }

    pub(super) fn git_cookie_registered(&self) {
        let mut inner = self.inner.lock();
        Self::increment(&mut inner.state.outstanding_git_cookies);
    }

    pub(super) fn git_cookie_revoked(&self) {
        let mut inner = self.inner.lock();
        Self::decrement(&mut inner.state.outstanding_git_cookies);
        self.quiescent.notify_all();
    }

    pub(super) fn git_cookie_revocation_deferred(&self, cookie: NonZeroU32) {
        let mut inner = self.inner.lock();
        Self::decrement(&mut inner.state.outstanding_git_cookies);
        Self::increment(&mut inner.state.revocation_debt);
        inner.git_revocation_debt.push(cookie);
        self.quiescent.notify_all();
    }

    pub(super) fn git_revocation_debt_resolved(&self) {
        let mut inner = self.inner.lock();
        Self::decrement(&mut inner.state.revocation_debt);
        self.quiescent.notify_all();
    }

    pub(super) fn claim_git_revocation_debt_batch(&self) -> Vec<GitRevocationDebtClaim> {
        let cookies = {
            let mut inner = self.inner.lock();
            std::mem::take(&mut inner.git_revocation_debt)
        };

        cookies
            .into_iter()
            .map(|cookie| GitRevocationDebtClaim {
                cookie: Some(cookie),
            })
            .collect()
    }

    pub(super) fn requeue_git_revocation_debt(&self, cookie: NonZeroU32) {
        let mut inner = self.inner.lock();
        // The debt count already includes this claim, so only return its
        // ownership to the queue. Do not change the count here.
        inner.git_revocation_debt.push(cookie);
    }

    fn increment(counter: &mut usize) {
        let Some(next) = counter.checked_add(1) else {
            std::process::abort();
        };
        *counter = next;
    }

    fn decrement(counter: &mut usize) {
        let Some(next) = counter.checked_sub(1) else {
            std::process::abort();
        };
        *counter = next;
    }

    pub(super) fn enter_call(&'static self) -> (ComModuleCallGuard, bool) {
        let (ingress_guard, accepted) = crate::module_runtime::ingress().enter_with(|| {
            #[cfg(any(test, feature = "refinement"))]
            self.record_ghost_event(crate::shutdown_refinement::GhostEvent::BeginRtdOperation);
        });
        let mut inner = self.inner.lock();
        Self::increment(&mut inner.state.in_flight_calls);
        drop(inner);
        (
            ComModuleCallGuard {
                lifetime: self,
                _ingress_guard: ingress_guard,
                #[cfg(any(test, feature = "refinement"))]
                record_ghost: accepted,
            },
            accepted,
        )
    }

    fn object_created(&self, kind: ComObjectKind) {
        let mut inner = self.inner.lock();
        match kind {
            ComObjectKind::Factory => Self::increment(&mut inner.state.live_factories),
            ComObjectKind::Server => Self::increment(&mut inner.state.live_servers),
        }
        drop(inner);
        #[cfg(any(test, feature = "refinement"))]
        self.record_ghost_event(match kind {
            ComObjectKind::Factory => crate::shutdown_refinement::GhostEvent::AddRtdClassFactory,
            ComObjectKind::Server => crate::shutdown_refinement::GhostEvent::AddRtdServer,
        });
    }

    fn object_destroyed(&self, kind: ComObjectKind) {
        let mut inner = self.inner.lock();
        match kind {
            ComObjectKind::Factory => Self::decrement(&mut inner.state.live_factories),
            ComObjectKind::Server => Self::decrement(&mut inner.state.live_servers),
        }
        drop(inner);
        #[cfg(any(test, feature = "refinement"))]
        self.record_ghost_event(match kind {
            ComObjectKind::Factory => crate::shutdown_refinement::GhostEvent::RemoveRtdClassFactory,
            ComObjectKind::Server => crate::shutdown_refinement::GhostEvent::RemoveRtdServer,
        });
        self.quiescent.notify_all();
    }

    pub(super) fn set_server_lock(&self, lock: bool) -> bool {
        let mut inner = self.inner.lock();
        let changed = if lock {
            Self::increment(&mut inner.state.server_locks);
            true
        } else if inner.state.server_locks == 0 {
            false
        } else {
            Self::decrement(&mut inner.state.server_locks);
            self.quiescent.notify_all();
            true
        };
        drop(inner);
        #[cfg(any(test, feature = "refinement"))]
        if changed {
            self.record_ghost_event(if lock {
                crate::shutdown_refinement::GhostEvent::LockRtdServer
            } else {
                crate::shutdown_refinement::GhostEvent::UnlockRtdServer
            });
        }
        changed
    }

    pub(super) fn can_unload_now(&self) -> bool {
        self.inner.lock().state.is_quiescent()
    }

    pub(super) fn wait_for_quiescence(
        &self,
        retry_git_revocation_debt: fn(),
    ) -> Result<(), ComModuleQuiescenceError> {
        #[cfg(test)]
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);

        loop {
            retry_git_revocation_debt();

            let mut inner = self.inner.lock();
            if inner.state.is_quiescent() {
                return Ok(());
            }
            if inner.state.has_only_git_blockers() {
                return Err(ComModuleQuiescenceError { state: inner.state });
            }

            #[cfg(test)]
            {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    return Err(ComModuleQuiescenceError { state: inner.state });
                }
                if self.quiescent.wait_for(&mut inner, remaining).timed_out() {
                    return Err(ComModuleQuiescenceError { state: inner.state });
                }
            }

            #[cfg(not(test))]
            self.quiescent.wait(&mut inner);
        }
    }

    #[cfg(test)]
    pub(super) fn snapshot(&self) -> ComModuleState {
        self.inner.lock().state
    }

    #[cfg(test)]
    pub(super) fn queued_git_revocation_debt(&self) -> Vec<NonZeroU32> {
        self.inner.lock().git_revocation_debt.clone()
    }
}

pub(super) struct ComModuleCallGuard {
    lifetime: &'static ComModuleLifetime,
    _ingress_guard: crate::ingress::ExportCallGuard<'static>,
    #[cfg(any(test, feature = "refinement"))]
    record_ghost: bool,
}

impl Drop for ComModuleCallGuard {
    fn drop(&mut self) {
        let mut inner = self.lifetime.inner.lock();
        ComModuleLifetime::decrement(&mut inner.state.in_flight_calls);
        self.lifetime.quiescent.notify_all();
        drop(inner);
        #[cfg(any(test, feature = "refinement"))]
        if self.record_ghost {
            self.lifetime
                .record_ghost_event(crate::shutdown_refinement::GhostEvent::EndRtdOperation);
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum ComObjectKind {
    Factory,
    Server,
}

pub(super) struct ComObjectLease {
    kind: ComObjectKind,
}

impl ComObjectLease {
    pub(super) fn new(kind: ComObjectKind) -> Self {
        super::module_lifetime().object_created(kind);
        Self { kind }
    }
}

impl Drop for ComObjectLease {
    fn drop(&mut self) {
        super::module_lifetime().object_destroyed(self.kind);
    }
}
