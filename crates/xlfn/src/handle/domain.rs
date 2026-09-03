//! Call-scoped handle read domain and deferred reclamation.
//!
//! Rather than acquiring a per-record reader permit on every lookup, UDF
//! calls register once with this domain for the duration of their [`CallScope`].
//! Readers enter striped drain gates partitioned by generation, avoiding
//! cache-line contention on individual binding records during concurrent
//! lookups of the same handle.
//!
//! When bindings are removed or retired, reclamation alternates the active
//! generation and quiesces the prior generation, ensuring that all in-flight
//! readers have completed before object capabilities are retired.

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
            generations: [StripedDrainGate::new_open(), StripedDrainGate::new_open()],
            current: AtomicUsize::new(0),
            reclaim_lock: Mutex::new(()),
            closed: AtomicBool::new(false),
        }
    }

    #[inline]
    pub(crate) fn enter(&self) -> XllResult<HandleDomainPermit> {
        if self.closed.load(Ordering::Acquire) {
            return Err(XllError::Closing);
        }
        let gen_idx = self.current.load(Ordering::Acquire) & 1;
        let stripe = xlfn_kernel::drain_gate::current_thread_stripe();
        self.generations[gen_idx]
            .try_acquire(stripe)
            .map_err(|_| XllError::Closing)?;
        Ok(HandleDomainPermit {
            gate: NonNull::from(&self.generations[gen_idx]),
            stripe,
        })
    }

    /// Waits for all in-flight readers that entered prior to this call to
    /// complete, guaranteeing safe reclamation of retired object capabilities.
    pub(crate) fn quiesce(&self) {
        let _reclaim = self.reclaim_lock.lock();
        let old_gen = self.current.fetch_xor(1, Ordering::SeqCst) & 1;
        self.generations[old_gen].wait_until_idle();
    }

    /// Seals both generations of the domain, preventing new readers and draining
    /// existing ones.
    pub(crate) fn seal(&self) {
        self.closed.store(true, Ordering::Release);
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
