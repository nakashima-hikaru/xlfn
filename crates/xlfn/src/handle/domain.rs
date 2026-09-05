//! Call-scoped handle read domain and deferred reclamation.
//!
//! Rather than acquiring a per-record reader permit on every lookup, UDF
//! calls register once with this domain for the duration of their [`CallScope`].
//! Readers enter striped drain gates partitioned by generation, avoiding
//! cache-line contention on individual binding records during concurrent
//! lookups of the same handle.
//!
//! When bindings are removed or retired, reclamation seals the active
//! generation, reopens the other generation, and then drains the sealed one.
//! Only the current generation is open, so a reader that observed the old
//! generation before rotation cannot be admitted after the grace period starts.

#![allow(
    unsafe_code,
    reason = "Domain permits wrap audited non-owning stripe leases tied to CallScope"
)]

use crate::{XllError, XllResult};
use parking_lot::Mutex;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use xlfn_kernel::drain_gate::{DEFAULT_STRIPE_COUNT, StripedDrainGate};

pub(crate) struct HandleReadDomain {
    generations: [StripedDrainGate<DEFAULT_STRIPE_COUNT>; 2],
    current: AtomicUsize,
    reclaim_lock: Mutex<()>,
    closed: AtomicBool,
}

impl HandleReadDomain {
    pub(crate) const fn new() -> Self {
        Self {
            generations: [StripedDrainGate::new_open(), StripedDrainGate::new_sealed()],
            current: AtomicUsize::new(0),
            reclaim_lock: Mutex::new(()),
            closed: AtomicBool::new(false),
        }
    }

    #[inline]
    pub(crate) fn enter(&self) -> XllResult<HandleDomainPermit> {
        let stripe = xlfn_kernel::drain_gate::current_thread_stripe();
        loop {
            if self.closed.load(Ordering::Acquire) {
                return Err(XllError::Closing);
            }
            let gen_idx = self.current.load(Ordering::Acquire) & 1;
            match self.generations[gen_idx].try_acquire(stripe) {
                Ok(()) => {
                    return Ok(HandleDomainPermit {
                        gate: NonNull::from(&self.generations[gen_idx]),
                        stripe,
                    });
                }
                Err(_) if !self.closed.load(Ordering::Acquire) => {
                    std::hint::spin_loop();
                }
                Err(_) => return Err(XllError::Closing),
            }
        }
    }

    /// Rotates the read generation and waits for all readers admitted to the
    /// sealed generation to complete.
    pub(crate) fn quiesce(&self) {
        let _reclaim = self.reclaim_lock.lock();
        if self.closed.load(Ordering::Acquire) {
            self.generations[0].seal_and_wait();
            self.generations[1].seal_and_wait();
            return;
        }
        let old_gen = self.current.load(Ordering::Acquire) & 1;
        let next_gen = old_gen ^ 1;
        self.generations[old_gen].seal();
        self.generations[next_gen]
            .reopen()
            .unwrap_or_else(|_| xlfn_kernel::invariant::fail_stop());
        self.current.store(next_gen, Ordering::Release);
        self.generations[old_gen].wait_until_idle();
    }

    /// Seals both generations of the domain, preventing new readers and
    /// draining existing ones.
    pub(crate) fn seal(&self) {
        self.closed.store(true, Ordering::Release);
        let _reclaim = self.reclaim_lock.lock();
        self.generations[0].seal_and_wait();
        self.generations[1].seal_and_wait();
    }
}

/// An RAII permit proving that the calling thread is inside an active read
/// generation.
pub(crate) struct HandleDomainPermit {
    gate: NonNull<StripedDrainGate<DEFAULT_STRIPE_COUNT>>,
    stripe: usize,
}

// SAFETY: HandleDomainPermit wraps a stripe lease inside StripedDrainGate and can be moved across threads.
unsafe impl Send for HandleDomainPermit {}
// SAFETY: HandleDomainPermit does not expose interior mutability and can be referenced across threads.
unsafe impl Sync for HandleDomainPermit {}

impl Drop for HandleDomainPermit {
    #[inline]
    fn drop(&mut self) {
        // SAFETY: `gate` points to a StripedDrainGate within HandleReadDomain,
        // which outlives the CallScope holding this permit.
        unsafe { self.gate.as_ref() }.release(self.stripe);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, mpsc};
    use std::time::{Duration, Instant};

    #[test]
    fn starts_with_only_the_current_generation_open() {
        let domain = HandleReadDomain::new();

        assert_eq!(domain.current.load(Ordering::Acquire), 0);
        assert!(!domain.generations[0].is_sealed());
        assert!(domain.generations[1].is_sealed());
    }

    #[test]
    fn rotation_seals_old_generation_before_draining_it() {
        let domain = Arc::new(HandleReadDomain::new());
        let permit = domain.enter().expect("initial generation is open");
        let (started_tx, started_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let rotating = Arc::clone(&domain);
        let worker = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            rotating.quiesce();
            finished_tx.send(()).unwrap();
        });

        started_rx.recv().unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while domain.current.load(Ordering::Acquire) != 1 {
            assert!(
                Instant::now() < deadline,
                "rotation did not publish next generation"
            );
            std::thread::yield_now();
        }
        assert!(domain.generations[0].is_sealed());
        assert!(!domain.generations[1].is_sealed());
        assert!(finished_rx.try_recv().is_err());

        drop(permit);
        finished_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("rotation must finish after the old reader exits");
        worker.join().unwrap();
    }

    #[test]
    fn old_generation_rejects_late_admission_after_rotation() {
        let domain = HandleReadDomain::new();
        let old = domain.current.load(Ordering::Acquire) & 1;
        let stripe = xlfn_kernel::drain_gate::current_thread_stripe();

        domain.quiesce();

        assert_eq!(domain.current.load(Ordering::Acquire) & 1, old ^ 1);
        assert!(domain.generations[old].try_acquire(stripe).is_err());
        drop(domain.enter().expect("next generation is open"));
    }

    #[test]
    fn closed_domain_quiesce_does_not_reopen_a_generation() {
        let domain = HandleReadDomain::new();

        domain.seal();
        domain.quiesce();

        assert!(domain.generations[0].is_sealed());
        assert!(domain.generations[1].is_sealed());
        assert!(matches!(domain.enter(), Err(XllError::Closing)));
    }
}
