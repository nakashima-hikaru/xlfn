use super::event::ManualResetEvent;
use super::update_event::ComApartmentGuard;
use crate::sync::Mutex;
use std::num::NonZeroU32;
use triomphe::Arc;
use xlfn_kernel::rollback::{Rollback, RollbackGuard};

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
    fn has_external_references(self) -> bool {
        self.live_factories != 0 || self.live_servers != 0 || self.server_locks != 0
    }

    pub(super) fn is_quiescent(self) -> bool {
        !self.has_external_references() && self.is_logically_quiescent()
    }

    fn is_logically_quiescent(self) -> bool {
        self.in_flight_calls == 0 && self.outstanding_git_cookies == 0 && self.revocation_debt == 0
    }

    fn has_only_git_blockers(self) -> bool {
        self.in_flight_calls == 0
            && (self.outstanding_git_cookies != 0 || self.revocation_debt != 0)
    }

    #[cfg(any(test, feature = "refinement"))]
    fn trace_snapshot(self) -> crate::excel_rtd_protocol::RtdTraceSnapshot {
        crate::excel_rtd_protocol::RtdTraceSnapshot {
            rtd_class_factories: self.live_factories as u64,
            rtd_servers: self.live_servers as u64,
            rtd_server_locks: self.server_locks as u64,
        }
    }
}

struct ComModuleLifetimeInner {
    state: ComModuleState,
    /// Cookies whose GIT revocation failed. The corresponding
    /// `state.revocation_debt` count also includes claims temporarily removed
    /// from this queue while a retry is in flight.
    git_revocation_debt: Vec<NonZeroU32>,
    quiescence_changed: Option<Arc<ManualResetEvent>>,
}

impl ComModuleLifetimeInner {
    fn notify_waiter(&self) {
        if let Some(event) = &self.quiescence_changed {
            event
                .set()
                .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop());
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ComModuleQuiescenceError {
    pub(super) state: ComModuleState,
}

pub(super) struct GitRevocationDebtClaim<'module> {
    pending: RollbackGuard<PendingGitRevocation<'module>>,
}

struct PendingGitRevocation<'module> {
    module: &'module ComModuleLifetime,
    cookie: NonZeroU32,
}

impl Rollback for PendingGitRevocation<'_> {
    fn rollback(self) {
        self.module.requeue_git_revocation_debt(self.cookie);
    }
}

impl GitRevocationDebtClaim<'_> {
    pub(super) fn cookie(&self) -> NonZeroU32 {
        self.pending.get().cookie
    }

    pub(super) fn resolve(self) {
        let PendingGitRevocation { module, .. } = self.pending.complete();
        module.git_revocation_debt_resolved();
    }
}

pub(crate) struct ComModuleLifetime {
    inner: Mutex<ComModuleLifetimeInner>,
    pub(super) observer: crate::shutdown_trace::ObservationSink,
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
                quiescence_changed: None,
            }),
            observer: crate::shutdown_trace::ObservationSink::new(),
        }
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(super) fn trace_snapshot(&self) -> crate::excel_rtd_protocol::RtdTraceSnapshot {
        self.inner.lock().state.trace_snapshot()
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(super) fn connect_trace(
        &self,
        trace: crate::shutdown_trace::ShutdownTraceHandle,
        initialize: impl FnOnce(crate::excel_rtd_protocol::RtdTraceSnapshot) -> crate::XllResult<()>,
    ) -> crate::XllResult<()> {
        // Late COM releases belong either to the old trace or the new initial
        // snapshot. Serialize the handoff with both the counters and events.
        let inner = self.inner.lock();
        initialize(inner.state.trace_snapshot())?;
        self.observer.set_trace_sink(trace);
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn set_trace_sink(&self, trace: crate::shutdown_trace::ShutdownTraceHandle) {
        self.observer.set_trace_sink(trace);
    }

    #[cfg(test)]
    pub(super) fn disable_trace_for_test(&self) {
        self.observer.disable_for_test();
    }

    fn record_shutdown_event(&self, event: crate::shutdown_trace::ShutdownEvent) {
        self.observer.record(event);
    }

    pub(super) fn git_cookie_registered(&self) {
        let mut inner = self.inner.lock();
        Self::increment(&mut inner.state.outstanding_git_cookies);
    }

    pub(super) fn git_cookie_revoked(&self) {
        let mut inner = self.inner.lock();
        Self::decrement(&mut inner.state.outstanding_git_cookies);
        inner.notify_waiter();
    }

    pub(super) fn git_cookie_revocation_deferred(&self, cookie: NonZeroU32) {
        let mut inner = self.inner.lock();
        Self::decrement(&mut inner.state.outstanding_git_cookies);
        Self::increment(&mut inner.state.revocation_debt);
        inner.git_revocation_debt.push(cookie);
        inner.notify_waiter();
    }

    fn git_revocation_debt_resolved(&self) {
        let mut inner = self.inner.lock();
        Self::decrement(&mut inner.state.revocation_debt);
        inner.notify_waiter();
    }

    pub(super) fn claim_git_revocation_debt_batch(&self) -> Vec<GitRevocationDebtClaim<'_>> {
        let cookies = {
            let mut inner = self.inner.lock();
            std::mem::take(&mut inner.git_revocation_debt)
        };

        cookies
            .into_iter()
            .map(|cookie| GitRevocationDebtClaim {
                pending: RollbackGuard::new(PendingGitRevocation {
                    module: self,
                    cookie,
                }),
            })
            .collect()
    }

    fn requeue_git_revocation_debt(&self, cookie: NonZeroU32) {
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
        let ingress_guard = crate::module_runtime::ingress().enter_with(|| {});
        let accepted = matches!(&ingress_guard, crate::ingress::ExportEntry::Admitted(_));
        let mut inner = self.inner.lock();
        Self::increment(&mut inner.state.in_flight_calls);
        drop(inner);
        (
            ComModuleCallGuard {
                lifetime: self,
                _thread_export: crate::ingress::ThreadExportGuard::enter(),
                _ingress_guard: ingress_guard,
                _observation: ComModuleCallObservation::begin(self, accepted),
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
        self.record_shutdown_event(match kind {
            ComObjectKind::Factory => crate::shutdown_trace::ShutdownEvent::AddRtdClassFactory,
            ComObjectKind::Server => crate::shutdown_trace::ShutdownEvent::AddRtdServer,
        });
    }

    fn object_destroyed(&self, kind: ComObjectKind) {
        let mut inner = self.inner.lock();
        match kind {
            ComObjectKind::Factory => Self::decrement(&mut inner.state.live_factories),
            ComObjectKind::Server => Self::decrement(&mut inner.state.live_servers),
        }
        self.record_shutdown_event(match kind {
            ComObjectKind::Factory => crate::shutdown_trace::ShutdownEvent::RemoveRtdClassFactory,
            ComObjectKind::Server => crate::shutdown_trace::ShutdownEvent::RemoveRtdServer,
        });
        inner.notify_waiter();
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
            inner.notify_waiter();
            true
        };
        if changed {
            self.record_shutdown_event(if lock {
                crate::shutdown_trace::ShutdownEvent::LockRtdServer
            } else {
                crate::shutdown_trace::ShutdownEvent::UnlockRtdServer
            });
        }
        changed
    }

    pub(super) fn can_unload_now(&self) -> bool {
        self.inner.lock().state.is_quiescent()
    }

    /// Called only after server shutdown has drained operations, detached
    /// generation backends, and joined notification/termination workers.
    /// Excel may retain terminal COM shells until xlAutoClose returns, so
    /// external references require residency, not a blocking physical drain.
    pub(super) fn wait_for_logical_quiescence(
        &self,
        mut retry_git_revocation_debt: impl FnMut(),
        module_residency_held: bool,
    ) -> Result<(), ComModuleQuiescenceError> {
        #[cfg(not(test))]
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        #[cfg(test)]
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);

        loop {
            retry_git_revocation_debt();

            let mut inner = self.inner.lock();
            if inner.state.is_logically_quiescent() {
                return if !inner.state.has_external_references() || module_residency_held {
                    Ok(())
                } else {
                    Err(ComModuleQuiescenceError { state: inner.state })
                };
            }
            if inner.state.has_only_git_blockers() {
                return Err(ComModuleQuiescenceError { state: inner.state });
            }

            // The shutdown transaction is the sole waiter. Arm its event
            // under the call-count lock so the last caller cannot miss it.
            if inner.quiescence_changed.is_some() {
                return Err(ComModuleQuiescenceError { state: inner.state });
            }
            let event = ManualResetEvent::new(false)
                .map(Arc::new)
                .map_err(|_| ComModuleQuiescenceError { state: inner.state })?;
            inner.quiescence_changed = Some(Arc::clone(&event));
            drop(inner);
            let event = std::mem::DropGuard::new(event, |_| {
                self.inner.lock().quiescence_changed = None;
            });
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(ComModuleQuiescenceError {
                    state: self.inner.lock().state,
                });
            }
            let timeout_ms = u32::try_from(remaining.as_millis()).unwrap_or(u32::MAX);
            // Preserve a caller's STA and dispatch synchronous COM calls
            // without the call-count lock. Initialize an MTA on other callers.
            let result = ComApartmentGuard::enter()
                .and_then(|_apartment| event.wait_with_com_pumping_for(timeout_ms));
            drop(event);
            if result.is_err() {
                return Err(ComModuleQuiescenceError {
                    state: self.inner.lock().state,
                });
            }
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
    _thread_export: crate::ingress::ThreadExportGuard,
    _ingress_guard: crate::ingress::ExportEntry<'static>,
    _observation: ComModuleCallObservation,
}

impl Drop for ComModuleCallGuard {
    fn drop(&mut self) {
        let mut inner = self.lifetime.inner.lock();
        ComModuleLifetime::decrement(&mut inner.state.in_flight_calls);
        inner.notify_waiter();
    }
}

struct ComModuleCallObservation {
    #[cfg(any(test, feature = "refinement"))]
    lifetime: &'static ComModuleLifetime,
    #[cfg(any(test, feature = "refinement"))]
    record_end: bool,
}

impl ComModuleCallObservation {
    fn begin(lifetime: &'static ComModuleLifetime, accepted: bool) -> Self {
        #[cfg(any(test, feature = "refinement"))]
        if accepted {
            lifetime.record_shutdown_event(crate::shutdown_trace::ShutdownEvent::BeginRtdOperation);
        }
        #[cfg(not(any(test, feature = "refinement")))]
        let _ = (lifetime, accepted);
        Self {
            #[cfg(any(test, feature = "refinement"))]
            lifetime,
            #[cfg(any(test, feature = "refinement"))]
            record_end: accepted,
        }
    }
}

impl Drop for ComModuleCallObservation {
    fn drop(&mut self) {
        #[cfg(any(test, feature = "refinement"))]
        if self.record_end {
            self.lifetime
                .record_shutdown_event(crate::shutdown_trace::ShutdownEvent::EndRtdOperation);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_revocation_claim_returns_to_and_resolves_its_issuer() {
        let issuer = ComModuleLifetime::new();
        let other = ComModuleLifetime::new();
        let cookie = NonZeroU32::new(41).unwrap();
        for module in [&issuer, &other] {
            module.git_cookie_registered();
            module.git_cookie_revocation_deferred(cookie);
        }

        let claim = issuer.claim_git_revocation_debt_batch().pop().unwrap();
        assert_eq!(claim.cookie(), cookie);
        assert!(issuer.queued_git_revocation_debt().is_empty());
        assert!(!issuer.can_unload_now());
        drop(claim);
        assert_eq!(issuer.queued_git_revocation_debt(), [cookie]);
        assert_eq!(issuer.snapshot().revocation_debt, 1);

        issuer
            .claim_git_revocation_debt_batch()
            .pop()
            .unwrap()
            .resolve();
        assert!(issuer.can_unload_now());
        assert_eq!(other.queued_git_revocation_debt(), [cookie]);
        assert!(!other.can_unload_now());
        other
            .claim_git_revocation_debt_batch()
            .pop()
            .unwrap()
            .resolve();
        assert!(other.can_unload_now());
    }
}
