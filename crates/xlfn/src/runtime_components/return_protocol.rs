//! Excel return ownership and call/calculation identity protocol.

use crate::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

// A stamp is never reused, including when a ReturnProtocol is moved or its
// address is reused. Only lifecycle management issues stamps; admission is unchanged.
static NEXT_RETURN_EPOCH: AtomicU64 = AtomicU64::new(1);

fn next_return_epoch() -> u64 {
    NEXT_RETURN_EPOCH
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1)
        })
        .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop())
}

/// A seal for one admission epoch, tied to its actual protocol until waiting.
pub(crate) struct ClosedReturns<'protocol> {
    protocol: &'protocol ReturnProtocol,
    epoch: u64,
}

/// A drained observation. It must be checked against its issuer's current
/// epoch when consumed; reopening deliberately invalidates this capability.
pub(crate) struct ReturnsDrained {
    issuer: usize,
    epoch: u64,
}

impl ClosedReturns<'_> {
    pub(crate) fn wait(self) -> crate::XllResult<ReturnsDrained> {
        self.wait_locked(|| {})
    }

    fn wait_locked(self, on_locked: impl FnOnce()) -> crate::XllResult<ReturnsDrained> {
        let epoch = self.protocol.admission_epoch.lock();
        if *epoch != self.epoch || !self.protocol.returns.admission_closed() {
            return Err(return_quiescence_error());
        }
        // This is management exclusion, not the gate's notification mutex.
        // Final release can still complete while this lock excludes reopen.
        on_locked();
        self.protocol.returns.wait_for_quiescence();
        Ok(ReturnsDrained {
            issuer: std::ptr::from_ref(self.protocol).addr(),
            epoch: self.epoch,
        })
    }
}

impl ReturnsDrained {
    pub(crate) fn matches(&self, protocol: &ReturnProtocol) -> bool {
        let epoch = protocol.admission_epoch.lock();
        self.issuer == std::ptr::from_ref(protocol).addr()
            && self.epoch == *epoch
            && protocol.returns.admission_closed()
            && protocol.returns.is_quiescent()
    }
}

fn return_quiescence_error() -> crate::XllError {
    crate::XllError::Internal {
        diagnostic_id: crate::diagnostics::id::DiagnosticId::CLOSE_CERTIFICATE,
    }
}

/// Excel return ownership and call/calculation identity state.
pub(crate) struct ReturnProtocol {
    returns: crate::return_abi::ReturnTracker,
    admission_epoch: Mutex<u64>,
    pub(crate) next_call_id: AtomicU64,
    #[cfg(not(feature = "async"))]
    pub(crate) calculation_id: AtomicU64,
}

impl ReturnProtocol {
    pub(crate) const fn new() -> Self {
        Self {
            returns: crate::return_abi::ReturnTracker::new_closed(),
            admission_epoch: Mutex::new(0),
            next_call_id: AtomicU64::new(1),
            #[cfg(not(feature = "async"))]
            calculation_id: AtomicU64::new(1),
        }
    }

    pub(crate) fn close_admission(&self) -> ClosedReturns<'_> {
        let mut epoch = self.admission_epoch.lock();
        if *epoch == 0 {
            *epoch = next_return_epoch();
        }
        self.returns.close_admission();
        ClosedReturns {
            protocol: self,
            epoch: *epoch,
        }
    }

    pub(crate) fn reopen_admission(&self) -> crate::XllResult<()> {
        let mut epoch = self.admission_epoch.lock();
        self.returns.reopen_admission()?;
        *epoch = next_return_epoch();
        Ok(())
    }

    pub(crate) fn enter_producer(
        &'static self,
    ) -> Option<crate::return_abi::ReturnProducerGuard<'static>> {
        self.returns.try_enter_producer()
    }

    pub(crate) fn wait_for_returns(&self) {
        self.returns.wait_for_quiescence();
    }

    pub(crate) fn returns_are_quiescent(&self) -> bool {
        self.returns.is_quiescent()
    }

    #[cfg(test)]
    pub(crate) fn returns_closed_and_quiescent(&self) -> bool {
        self.returns.admission_closed() && self.returns.is_quiescent()
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn set_trace_sink(&self, trace: crate::shutdown_trace::ShutdownTraceHandle) {
        self.returns.set_trace_sink(trace);
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn outstanding_obligations(&self) -> usize {
        self.returns.outstanding_obligations()
    }

    #[cfg(test)]
    pub(crate) fn test_enter_producer(&self) -> Option<crate::return_abi::ReturnProducerGuard<'_>> {
        self.returns.try_enter_producer()
    }

    pub(crate) fn next_call_id(&self) -> u64 {
        self.next_call_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn peek_next_call_id(&self) -> u64 {
        self.next_call_id.load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;

    #[test]
    fn miri_return_drain_rejects_another_initial_or_opened_protocol() {
        let first = ReturnProtocol::new();
        let second = ReturnProtocol::new();
        let drained = first.close_admission().wait().unwrap();
        assert!(drained.matches(&first));
        assert!(!drained.matches(&second));
        let other = second.close_admission().wait().unwrap();
        assert!(!other.matches(&first));
        assert!(other.matches(&second));
        second.reopen_admission().unwrap();
        assert!(!drained.matches(&second));
    }

    #[test]
    fn miri_return_reopen_invalidates_both_old_seals_and_completed_drains() {
        let protocol = ReturnProtocol::new();
        protocol.reopen_admission().unwrap();
        let old_seal = protocol.close_admission();
        let old_drain = protocol.close_admission().wait().unwrap();
        protocol.reopen_admission().unwrap();
        let current = protocol.close_admission().wait().unwrap();
        assert!(old_seal.wait().is_err());
        assert!(!old_drain.matches(&protocol));
        assert!(current.matches(&protocol));
    }

    #[test]
    fn miri_return_drain_cannot_authorize_reused_storage_or_moved_issuers() {
        let mut storage = Box::new(ReturnProtocol::new());
        let original_address = std::ptr::from_ref(&*storage).addr();
        let old_drain = storage.close_admission().wait().unwrap();
        *storage = ReturnProtocol::new();
        assert_eq!(std::ptr::from_ref(&*storage).addr(), original_address);
        let current = storage.close_admission().wait().unwrap();
        assert!(!old_drain.matches(&storage));
        assert!(current.matches(&storage));

        let local = ReturnProtocol::new();
        let drain_before_move = local.close_admission().wait().unwrap();
        let moved = Box::new(local);
        assert!(!drain_before_move.matches(&moved));
        assert!(moved.close_admission().wait().unwrap().matches(&moved));
    }

    #[test]
    fn miri_return_wait_excludes_reopen_without_blocking_final_release() {
        let protocol = ReturnProtocol::new();
        protocol.reopen_admission().unwrap();
        let producer = protocol.test_enter_producer().unwrap();
        let waiting = Barrier::new(2);
        std::thread::scope(|threads| {
            let waiter = threads.spawn(|| {
                protocol
                    .close_admission()
                    .wait_locked(|| {
                        waiting.wait();
                    })
                    .unwrap()
            });
            waiting.wait();
            // Reopen needs this management lock, while producer release only
            // needs the independent notification protocol owned by the gate.
            assert!(protocol.admission_epoch.try_lock().is_none());
            assert_eq!(protocol.outstanding_obligations(), 1);
            drop(producer);
            let drained = waiter.join().unwrap();
            assert!(drained.matches(&protocol));
            protocol.reopen_admission().unwrap();
            assert!(!drained.matches(&protocol));
        });
    }

    #[test]
    #[allow(
        clippy::mem_forget,
        clippy::forget_non_drop,
        reason = "forgetting a seal must retain closed admission"
    )]
    fn miri_forgetting_return_seal_never_reopens_admission() {
        let protocol = ReturnProtocol::new();
        protocol.reopen_admission().unwrap();
        std::mem::forget(protocol.close_admission());
        assert!(protocol.test_enter_producer().is_none());
        assert!(
            protocol
                .close_admission()
                .wait()
                .unwrap()
                .matches(&protocol)
        );
    }
}
