//! Excel return ownership and call/calculation identity protocol.

use crate::sync::Mutex;
use std::cell::Cell;
use std::num::NonZeroU64;
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
    issuer: Option<NonZeroU64>,
    next: NonZeroU64,
    end: NonZeroU64,
}

thread_local! {
    static CALL_ID_RANGE: Cell<CallIdRange> = const {
        Cell::new(CallIdRange {
            issuer: None,
            next: NonZeroU64::MIN,
            end: NonZeroU64::MIN,
        })
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
    fn initialize_issuer(&self) -> NonZeroU64 {
        let candidate = NEXT_CALL_ISSUER
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop());
        // The stamp carries identity only, not visibility of any runtime
        // state. Concurrent initializers agree on the first published value.
        let raw =
            match self
                .issuer
                .compare_exchange(0, candidate, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => candidate,
                Err(issuer) => issuer,
            };
        NonZeroU64::new(raw).unwrap_or_else(|| xlfn_kernel::invariant::fail_stop())
    }

    // MAX is the exhaustion sentinel, never a returned ID. Use the remaining
    // partial range rather than wrapping or failing a block early.
    fn reserve(&self, count: u64) -> Option<(NonZeroU64, NonZeroU64)> {
        let first = self
            .next_unreserved
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |first| {
                (first != u64::MAX).then_some(first.saturating_add(count))
            })
            .ok()?;
        let start = NonZeroU64::new(first)?;
        Some((start, start.saturating_add(count)))
    }

    #[cold]
    fn refill(&self, issuer: NonZeroU64, slot: &Cell<CallIdRange>) -> NonZeroU64 {
        let (start, end) = self
            .reserve(CALL_ID_BATCH)
            .unwrap_or_else(|| xlfn_kernel::invariant::fail_stop());
        slot.set(CallIdRange {
            issuer: Some(issuer),
            next: start.saturating_add(1),
            end,
        });
        start
    }

    #[inline]
    fn next(&self) -> NonZeroU64 {
        let issuer = match NonZeroU64::new(self.issuer.load(Ordering::Relaxed)) {
            Some(issuer) => issuer,
            None => self.initialize_issuer(),
        };
        CALL_ID_RANGE
            .try_with(|slot| {
                let mut range = slot.get();
                if range.issuer == Some(issuer) && range.next < range.end {
                    let id = range.next;
                    range.next = id.saturating_add(1);
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
                    .0
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

#[cfg(not(miri))]
const RETURN_QUIESCENCE_TIMEOUT: std::time::Duration = if cfg!(test) {
    std::time::Duration::from_secs(2)
} else {
    std::time::Duration::from_secs(10)
};

fn wait_for_returns_with_timeout(returns: &crate::return_abi::ReturnTracker) -> bool {
    #[cfg(miri)]
    {
        returns.wait_for_quiescence();
        true
    }
    #[cfg(not(miri))]
    {
        #[cfg(windows)]
        {
            use crate::drain_wait::{UnstartedDrain, drain_with_com_dispatch};
            match drain_with_com_dispatch(UnstartedDrain::new(move || {
                returns.wait_for_quiescence_timeout(RETURN_QUIESCENCE_TIMEOUT)
            })) {
                Ok(prepared) => match prepared.run() {
                    Ok(joined) => joined.into_output(),
                    Err((_error, unstarted)) => (unstarted.into_work())(),
                },
                Err((_error, unstarted)) => (unstarted.into_work())(),
            }
        }
        #[cfg(not(windows))]
        {
            returns.wait_for_quiescence_timeout(RETURN_QUIESCENCE_TIMEOUT)
        }
    }
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
        if !wait_for_returns_with_timeout(&self.protocol.returns) {
            return Err(return_quiescence_error());
        }
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
    crate::error::construct::internal(crate::diagnostics::catalog::CLOSE_CERTIFICATE)
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

    #[cfg(test)]
    pub(crate) fn wait_for_returns(&self) {
        self.returns.wait_for_quiescence();
    }

    pub(crate) fn wait_for_returns_timeout(&self) -> bool {
        wait_for_returns_with_timeout(&self.returns)
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

    pub(crate) fn next_call_id(&self) -> NonZeroU64 {
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
            assert_eq!(ids[0].get(), 1);
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
        assert_eq!(protocol.next_call_id().get(), 1);
        let issuer = protocol.call_ids.issuer.load(Ordering::Relaxed);
        *protocol = ReturnProtocol::new();
        assert_eq!(std::ptr::from_ref(&*protocol).addr(), address);
        assert_eq!(protocol.next_call_id().get(), 1);
        assert_ne!(protocol.call_ids.issuer.load(Ordering::Relaxed), issuer);
        protocol.reopen_admission().unwrap();
        assert_eq!(protocol.next_call_id().get(), 2);
    }

    #[test]
    fn call_id_partial_final_batch_does_not_wrap() {
        let ids = CallIds::new();
        ids.next_unreserved.store(u64::MAX - 2, Ordering::Relaxed);
        assert_eq!(ids.next().get(), u64::MAX - 2);
        assert_eq!(ids.next().get(), u64::MAX - 1);
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

    #[test]
    #[cfg(not(miri))]
    fn close_admission_wait_times_out_when_return_obligation_is_held() {
        let protocol = ReturnProtocol::new();
        protocol.reopen_admission().unwrap();
        let _leaked = protocol.test_enter_producer().unwrap();
        let err = protocol.close_admission().wait().err().unwrap();
        assert!(matches!(err, __err if __err.diagnostic_id().is_some()));
    }
}
