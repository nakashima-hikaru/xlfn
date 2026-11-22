//! Excel return ownership and call/calculation identity protocol.

use crate::sync::Mutex;
use std::cell::Cell;
use std::ops::Range;
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

// One identity per protocol, not per admission epoch. Ranges can survive
// close/reopen on the same runtime, but must never follow reused storage to a
// different runtime. Neither the thread-local slot nor its identity retains
// a pointer or a reference back to the protocol.
static NEXT_CALL_ISSUER: AtomicU64 = AtomicU64::new(1);
const CALL_ID_BATCH: u64 = 256;

struct CallIds {
    issuer: AtomicU64,
    next_unreserved: AtomicU64,
}

#[derive(Clone, Copy)]
struct CallIdRange {
    issuer: u64,
    next: u64,
    end: u64,
}

thread_local! {
    static CALL_ID_RANGE: Cell<CallIdRange> = const {
        Cell::new(CallIdRange { issuer: 0, next: 0, end: 0 })
    };
}

impl CallIds {
    const fn new() -> Self {
        Self {
            issuer: AtomicU64::new(0),
            next_unreserved: AtomicU64::new(1),
        }
    }

    #[cold]
    fn initialize_issuer(&self) -> u64 {
        let candidate = NEXT_CALL_ISSUER
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop());
        // The stamp carries identity only, not visibility of any runtime
        // state. Concurrent initializers agree on the first published value.
        match self
            .issuer
            .compare_exchange(0, candidate, Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(_) => candidate,
            Err(issuer) => issuer,
        }
    }

    // MAX is the exhaustion sentinel, never a returned ID. Use the remaining
    // partial range rather than wrapping or failing a block early.
    fn reserve(&self, count: u64) -> Option<Range<u64>> {
        let first = self
            .next_unreserved
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |first| {
                (first != u64::MAX).then_some(first.saturating_add(count))
            })
            .ok()?;
        Some(first..first.saturating_add(count))
    }

    #[cold]
    fn refill(&self, issuer: u64, slot: &Cell<CallIdRange>) -> u64 {
        let range = self
            .reserve(CALL_ID_BATCH)
            .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop());
        slot.set(CallIdRange {
            issuer,
            next: range.start + 1,
            end: range.end,
        });
        range.start
    }

    #[inline]
    fn next(&self) -> u64 {
        let issuer = match self.issuer.load(Ordering::Relaxed) {
            0 => self.initialize_issuer(),
            issuer => issuer,
        };
        CALL_ID_RANGE
            .try_with(|slot| {
                let mut range = slot.get();
                if range.issuer == issuer && range.next < range.end {
                    let id = range.next;
                    range.next += 1;
                    slot.set(range);
                    id
                } else {
                    self.refill(issuer, slot)
                }
            })
            .unwrap_or_else(|_| {
                // Keep correlation available even if invoked during TLS teardown.
                self.reserve(1)
                    .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop())
                    .start
            })
    }
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
    call_ids: CallIds,
    #[cfg(not(feature = "async"))]
    pub(crate) calculation_id: AtomicU64,
}

impl ReturnProtocol {
    pub(crate) const fn new() -> Self {
        Self {
            returns: crate::return_abi::ReturnTracker::new_closed(),
            admission_epoch: Mutex::new(0),
            call_ids: CallIds::new(),
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
        self.call_ids.next()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;

    #[test]
    fn call_id_ranges_stay_unique_across_concurrent_refills() {
        let protocol = ReturnProtocol::new();
        let start = Barrier::new(4);
        std::thread::scope(|scope| {
            let threads = (0..4)
                .map(|_| {
                    scope.spawn(|| {
                        start.wait();
                        (0..600)
                            .map(|_| protocol.next_call_id())
                            .collect::<Vec<_>>()
                    })
                })
                .collect::<Vec<_>>();
            let mut ids = threads
                .into_iter()
                .flat_map(|thread| {
                    let ids = thread.join().unwrap();
                    assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
                    ids
                })
                .collect::<Vec<_>>();
            ids.sort_unstable();
            ids.dedup();
            assert_eq!(ids.len(), 2400);
            assert_eq!(ids[0], 1);
        });
        assert_eq!(
            protocol.call_ids.next_unreserved.load(Ordering::Relaxed),
            1 + 12 * CALL_ID_BATCH
        );
    }

    #[test]
    fn call_id_slot_rejects_reused_protocol_storage() {
        let mut protocol = Box::new(ReturnProtocol::new());
        let address = std::ptr::from_ref(&*protocol).addr();
        assert_eq!(protocol.next_call_id(), 1);
        let issuer = protocol.call_ids.issuer.load(Ordering::Relaxed);
        *protocol = ReturnProtocol::new();
        assert_eq!(std::ptr::from_ref(&*protocol).addr(), address);
        assert_eq!(protocol.next_call_id(), 1);
        assert_ne!(protocol.call_ids.issuer.load(Ordering::Relaxed), issuer);
        protocol.reopen_admission().unwrap();
        assert_eq!(protocol.next_call_id(), 2);
    }

    #[test]
    fn call_id_partial_final_batch_does_not_wrap() {
        let ids = CallIds::new();
        ids.next_unreserved.store(u64::MAX - 2, Ordering::Relaxed);
        assert_eq!(ids.next(), u64::MAX - 2);
        assert_eq!(ids.next(), u64::MAX - 1);
        assert_eq!(ids.reserve(1), None);
    }

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
