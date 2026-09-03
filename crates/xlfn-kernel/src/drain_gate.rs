//! Single- and multi-stripe sealable admission gates.

use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use crossbeam_utils::CachePadded;
use parking_lot::{Condvar, Mutex};

use crate::sealable_counter::{ReleaseOutcome, ReopenError, SealableCounter, Sealed};

/// Default stripe count for scalable concurrency without false sharing or cache-line bouncing.
pub const DEFAULT_STRIPE_COUNT: usize = 32;

thread_local! {
    static THREAD_STRIPE: Cell<usize> = const { Cell::new(usize::MAX) };
}
static NEXT_STRIPE: AtomicUsize = AtomicUsize::new(0);

/// Returns the assigned stripe index in `[0, DEFAULT_STRIPE_COUNT)` for the calling thread.
///
/// Threads lazily receive a round-robin stripe assignment on first access, cached in TLS.
#[inline]
pub fn current_thread_stripe() -> usize {
    let current = THREAD_STRIPE.get();
    if current != usize::MAX {
        return current;
    }
    let assigned = NEXT_STRIPE.fetch_add(1, Ordering::Relaxed) & (DEFAULT_STRIPE_COUNT - 1);
    THREAD_STRIPE.set(assigned);
    assigned
}

/// A one-counter drain gate with lost-wakeup-safe waiting.
pub struct DrainGate {
    counter: SealableCounter,
    waiters: AtomicUsize,
    wait_lock: Mutex<()>,
    idle: Condvar,
}

impl DrainGate {
    pub const fn new_open() -> Self {
        Self {
            counter: SealableCounter::new_open(),
            waiters: AtomicUsize::new(0),
            wait_lock: Mutex::new(()),
            idle: Condvar::new(),
        }
    }

    pub const fn new_sealed() -> Self {
        Self {
            counter: SealableCounter::new_sealed(),
            waiters: AtomicUsize::new(0),
            wait_lock: Mutex::new(()),
            idle: Condvar::new(),
        }
    }

    #[inline]
    pub fn try_enter(&self) -> Result<DrainPermit<'_>, Sealed> {
        self.counter.try_acquire()?;
        Ok(DrainPermit { gate: self })
    }

    /// Acquires an owned permit from a process-lifetime gate.
    ///
    /// This is the temporal-lifetime counterpart of an owning reference: the
    /// gate owner must seal and drain every permit before reclaiming the
    /// pointed-to object.
    #[inline]
    pub fn try_enter_owned(&'static self) -> Result<OwnedDrainPermit, Sealed> {
        self.counter.try_acquire()?;
        Ok(OwnedDrainPermit { gate: self })
    }

    /// Acquires one count without retaining an RAII permit.
    ///
    /// Callers using this form must pair it with [`DrainGate::release`].
    #[inline]
    pub fn try_acquire(&self) -> Result<(), Sealed> {
        self.counter.try_acquire()
    }

    #[inline]
    pub fn release(&self) -> ReleaseOutcome {
        let outcome = self.counter.release();
        if self.waiters.load(Ordering::Relaxed) != 0 && outcome == ReleaseOutcome::BecameIdle {
            let _wait = self.wait_lock.lock();
            self.idle.notify_all();
        }
        outcome
    }

    #[inline]
    pub fn seal(&self) {
        self.counter.seal();
    }

    pub fn wait_until_idle(&self) {
        if self.active() == 0 {
            return;
        }
        self.waiters.fetch_add(1, Ordering::SeqCst);
        let mut wait = self.wait_lock.lock();
        while self.active() != 0 {
            self.idle.wait(&mut wait);
        }
        drop(wait);
        self.waiters.fetch_sub(1, Ordering::SeqCst);
    }

    pub fn seal_and_wait(&self) {
        self.seal();
        self.wait_until_idle();
    }

    #[inline]
    pub fn reopen(&self) -> Result<(), ReopenError> {
        self.counter.reopen()
    }

    #[inline]
    pub fn active(&self) -> usize {
        self.counter.active()
    }

    #[inline]
    pub fn is_sealed(&self) -> bool {
        self.counter.is_sealed()
    }
}

impl std::fmt::Debug for DrainGate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DrainGate")
            .field("sealed", &self.is_sealed())
            .field("active", &self.active())
            .finish()
    }
}

/// One active admission held in a [`DrainGate`].
#[derive(Debug)]
pub struct DrainPermit<'a> {
    gate: &'a DrainGate,
}

impl Drop for DrainPermit<'_> {
    fn drop(&mut self) {
        self.gate.release();
    }
}

/// An active admission whose gate has process lifetime.
#[derive(Debug)]
pub struct OwnedDrainPermit {
    gate: &'static DrainGate,
}

impl Drop for OwnedDrainPermit {
    fn drop(&mut self) {
        self.gate.release();
    }
}

/// A striped drain gate. Stripe selection remains a policy of the caller.
pub struct StripedDrainGate<const N: usize> {
    counters: [CachePadded<SealableCounter>; N],
    waiters: AtomicUsize,
    wait_lock: Mutex<()>,
    idle: Condvar,
}

impl<const N: usize> StripedDrainGate<N> {
    pub const fn new_open() -> Self {
        Self {
            counters: [const { CachePadded::new(SealableCounter::new_open()) }; N],
            waiters: AtomicUsize::new(0),
            wait_lock: Mutex::new(()),
            idle: Condvar::new(),
        }
    }

    pub const fn new_sealed() -> Self {
        Self {
            counters: [const { CachePadded::new(SealableCounter::new_sealed()) }; N],
            waiters: AtomicUsize::new(0),
            wait_lock: Mutex::new(()),
            idle: Condvar::new(),
        }
    }

    fn counter(&self, stripe: usize) -> &SealableCounter {
        self.counters
            .get(stripe)
            .expect("striped drain gate stripe index out of range")
    }

    #[inline]
    pub fn try_enter(&self, stripe: usize) -> Result<StripedDrainPermit<'_, N>, Sealed> {
        self.counter(stripe).try_acquire()?;
        Ok(StripedDrainPermit { gate: self, stripe })
    }

    /// Acquires one count on the given stripe without retaining an RAII permit.
    ///
    /// Callers using this form must pair it with [`StripedDrainGate::release`].
    #[inline]
    pub fn try_acquire(&self, stripe: usize) -> Result<(), Sealed> {
        self.counter(stripe).try_acquire()
    }

    #[inline]
    pub fn release(&self, stripe: usize) -> ReleaseOutcome {
        let counter = self.counter(stripe);
        let outcome = counter.release();
        if self.waiters.load(Ordering::Relaxed) != 0
            && outcome == ReleaseOutcome::BecameIdle
            && self.active() == 0
        {
            let _wait = self.wait_lock.lock();
            self.idle.notify_all();
        }
        outcome
    }

    pub fn seal(&self) {
        for counter in &self.counters {
            counter.seal();
        }
    }

    pub fn wait_until_idle(&self) {
        if self.active() == 0 {
            return;
        }
        self.waiters.fetch_add(1, Ordering::SeqCst);
        let mut wait = self.wait_lock.lock();
        while self.active() != 0 {
            self.idle.wait(&mut wait);
        }
        drop(wait);
        self.waiters.fetch_sub(1, Ordering::SeqCst);
    }

    pub fn seal_and_wait(&self) {
        self.seal();
        self.wait_until_idle();
    }

    pub fn reopen(&self) -> Result<(), ReopenError> {
        if self
            .counters
            .iter()
            .any(|counter| !counter.is_sealed() || counter.active() != 0)
        {
            return Err(ReopenError);
        }
        for counter in &self.counters {
            counter.reopen()?;
        }
        Ok(())
    }

    #[inline]
    pub fn active(&self) -> usize {
        self.counters.iter().fold(0, |active, counter| {
            active
                .checked_add(counter.active())
                .unwrap_or_else(|| crate::invariant::fail_stop())
        })
    }

    #[inline]
    pub fn is_sealed(&self) -> bool {
        self.counters.iter().all(|counter| counter.is_sealed())
    }
}

impl<const N: usize> StripedDrainGate<N> {
    #[inline]
    pub fn try_enter_owned(
        &'static self,
        stripe: usize,
    ) -> Result<StripedOwnedDrainPermit<N>, Sealed> {
        self.counter(stripe).try_acquire()?;
        Ok(StripedOwnedDrainPermit { gate: self, stripe })
    }
}

impl StripedDrainGate<DEFAULT_STRIPE_COUNT> {
    /// Attempts to enter using the calling thread's assigned stripe.
    #[inline]
    pub fn try_enter_current(
        &self,
    ) -> Result<StripedDrainPermit<'_, DEFAULT_STRIPE_COUNT>, Sealed> {
        self.try_enter(current_thread_stripe())
    }

    /// Attempts to enter and acquire an owned permit using the calling thread's assigned stripe.
    #[inline]
    pub fn try_enter_owned_current(
        &'static self,
    ) -> Result<StripedOwnedDrainPermit<DEFAULT_STRIPE_COUNT>, Sealed> {
        self.try_enter_owned(current_thread_stripe())
    }
}

impl<const N: usize> std::fmt::Debug for StripedDrainGate<N> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StripedDrainGate")
            .field("stripes", &N)
            .field("sealed", &self.is_sealed())
            .field("active", &self.active())
            .finish()
    }
}

/// One active admission held in a [`StripedDrainGate`].
#[derive(Debug)]
pub struct StripedDrainPermit<'a, const N: usize> {
    gate: &'a StripedDrainGate<N>,
    stripe: usize,
}

impl<const N: usize> Drop for StripedDrainPermit<'_, N> {
    fn drop(&mut self) {
        self.gate.release(self.stripe);
    }
}

/// An owned active admission held in a [`StripedDrainGate`].
#[derive(Debug)]
pub struct StripedOwnedDrainPermit<const N: usize> {
    gate: &'static StripedDrainGate<N>,
    stripe: usize,
}

impl<const N: usize> Drop for StripedOwnedDrainPermit<N> {
    fn drop(&mut self) {
        self.gate.release(self.stripe);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn drain_gate_waits_for_the_last_permit() {
        let gate = Arc::new(DrainGate::new_open());
        let permit = gate.try_enter().unwrap();
        gate.seal();

        let waiting = Arc::clone(&gate);
        let waiter = std::thread::spawn(move || waiting.wait_until_idle());
        std::thread::sleep(Duration::from_millis(5));
        assert!(!waiter.is_finished());
        drop(permit);
        waiter.join().unwrap();
    }

    #[test]
    fn striped_gate_reopens_only_after_all_stripes_drain() {
        let gate = StripedDrainGate::<2>::new_sealed();
        assert!(gate.try_enter(0).is_err());
        gate.reopen().unwrap();
        let first = gate.try_enter(0).unwrap();
        let second = gate.try_enter(1).unwrap();
        assert_eq!(gate.active(), 2);
        gate.seal();
        assert!(gate.reopen().is_err());
        drop(first);
        drop(second);
        gate.wait_until_idle();
        gate.reopen().unwrap();
        assert_eq!(gate.active(), 0);
    }
}
