//! A two-generation reader admission domain with an explicit grace-period
//! callback.
//!
//! The transition lock is held through the callback. This lets a subsystem
//! drain its retired work before another transition can reopen the generation
//! that was just drained.

#![allow(
    unsafe_code,
    reason = "owned permits are audited non-owning temporal capabilities"
)]

use crate::drain_gate::{DEFAULT_STRIPE_COUNT, StripedDrainGate, current_thread_stripe};
use parking_lot::Mutex;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// An opaque index identifying one of the two read generations.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct GenerationIndex(u8);

impl GenerationIndex {
    /// Returns the array index represented by this generation.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// The generation whose readers have drained and whose retired work may now
/// be processed by the transition callback.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DrainedGeneration {
    index: GenerationIndex,
}

impl DrainedGeneration {
    /// Returns the array index represented by this drained generation.
    #[must_use]
    pub const fn index(self) -> usize {
        self.index.index()
    }
}

/// Returned when a reader attempts to enter after the domain has been sealed.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DomainClosed;

/// A two-generation admission domain handle.
///
/// Between transitions, an open domain has exactly one open generation. A transition
/// seals the current generation before publishing the replacement, waits for
/// the sealed generation to become idle, and invokes its callback while the
/// transition lock remains held.
///
/// The protocol maintains these invariants:
///
/// - D1: at most one generation admits readers; between transitions, an open
///   domain has exactly one open generation. Both may be sealed briefly while
///   the replacement is being published.
/// - D2: readers are admitted only through the published current generation.
/// - D3: the current generation is sealed before the replacement is published.
/// - D4: the sealed generation is idle before the transition callback runs.
/// - D5: a closed domain never reopens a generation.
pub struct RotatingReadDomain<const N: usize> {
    generations: [StripedDrainGate<N>; 2],
    current: AtomicUsize,
    transition: Mutex<()>,
    closed: AtomicBool,
}

impl<const N: usize> RotatingReadDomain<N> {
    /// Creates an open domain with generation zero selected.
    #[must_use]
    pub fn new() -> Self {
        Self {
            generations: [StripedDrainGate::new_open(), StripedDrainGate::new_sealed()],
            current: AtomicUsize::new(0),
            transition: Mutex::new(()),
            closed: AtomicBool::new(false),
        }
    }

    /// Enters the currently published generation using `stripe`.
    ///
    /// If a transition seals the generation after it was selected, the
    /// acquisition is rejected and the reader retries against the newly
    /// published generation. This closes the load/seal late-admission race.
    #[inline]
    pub fn enter(&self, stripe: usize) -> Result<RotatingReadPermit<'_, N>, DomainClosed> {
        self.enter_impl(stripe, |_| {})
    }

    /// Enters the currently published generation with a permit whose storage
    /// is independent of the borrow of this domain.
    ///
    /// The caller must keep this domain alive until the returned permit is
    /// dropped. Call-scope owners satisfy that condition by being nested
    /// inside the generation owner that contains this domain.
    ///
    /// # Safety
    ///
    /// The caller must uphold the owner-lifetime requirement above.
    #[inline]
    pub unsafe fn enter_owned(
        &self,
        stripe: usize,
    ) -> Result<RotatingReadOwnedPermit<N>, DomainClosed> {
        loop {
            if self.closed.load(Ordering::Acquire) {
                return Err(DomainClosed);
            }
            let generation = self.current_generation();
            let gate = &self.generations[generation.index()];
            match gate.try_acquire(stripe) {
                Ok(()) => {
                    return Ok(RotatingReadOwnedPermit {
                        gate: NonNull::from(gate),
                        stripe,
                    });
                }
                Err(_) if !self.closed.load(Ordering::Acquire) => {
                    std::hint::spin_loop();
                }
                Err(_) => return Err(DomainClosed),
            }
        }
    }

    #[inline]
    fn enter_impl(
        &self,
        stripe: usize,
        after_generation_load: impl Fn(GenerationIndex),
    ) -> Result<RotatingReadPermit<'_, N>, DomainClosed> {
        loop {
            if self.closed.load(Ordering::Acquire) {
                return Err(DomainClosed);
            }
            let generation = self.current_generation();
            after_generation_load(generation);
            match self.generations[generation.index()].try_acquire(stripe) {
                Ok(()) => {
                    return Ok(RotatingReadPermit {
                        gate: &self.generations[generation.index()],
                        stripe,
                    });
                }
                Err(_) if !self.closed.load(Ordering::Acquire) => {
                    std::hint::spin_loop();
                }
                Err(_) => return Err(DomainClosed),
            }
        }
    }

    /// Test-only hook that pauses a reader after it loads the current
    /// generation and before it attempts to acquire that generation.
    #[cfg(test)]
    fn enter_with_hook(
        &self,
        stripe: usize,
        after_generation_load: impl Fn(GenerationIndex),
    ) -> Result<RotatingReadPermit<'_, N>, DomainClosed> {
        self.enter_impl(stripe, after_generation_load)
    }

    /// Returns the generation selected by the current publication.
    #[must_use]
    #[inline]
    pub fn current_generation(&self) -> GenerationIndex {
        GenerationIndex((self.current.load(Ordering::Acquire) & 1) as u8)
    }

    /// Rotates the read generation and runs `operation` after the old
    /// generation is sealed and idle.
    ///
    /// The transition lock remains held while `operation` runs. Subsystems
    /// must use this callback to drain retired work before another rotation is
    /// allowed.
    pub fn quiesce<R>(
        &self,
        operation: impl FnOnce(DrainedGeneration) -> R,
    ) -> Result<R, DomainClosed> {
        let _transition = self.transition.lock();
        self.rotate_and_run_locked(operation)
    }

    /// Best-effort form of [`Self::quiesce`] for maintenance paths that must
    /// not wait for another transition already in progress.
    ///
    /// Once the lock is acquired, this may wait for the old readers to drain.
    /// `None` means the transition lock was busy; `Some(Err(DomainClosed))`
    /// means the lock was acquired and closure was observed.
    pub fn try_quiesce<R>(
        &self,
        operation: impl FnOnce(DrainedGeneration) -> R,
    ) -> Option<Result<R, DomainClosed>> {
        let _transition = self.transition.try_lock()?;
        Some(self.rotate_and_run_locked(operation))
    }

    /// Attempts quiescence without waiting for the transition lock or readers.
    ///
    /// Each stripe's idle check and admission seal are atomic. A reader that
    /// wins admission causes the attempt to return `None`, with the current
    /// generation left open. On success, `operation` runs under the transition
    /// lock after the old generation is sealed and idle. The callback itself
    /// may block; callers needing bounded latency must keep it non-blocking.
    pub fn try_quiesce_if_idle<R>(
        &self,
        operation: impl FnOnce(DrainedGeneration) -> R,
    ) -> Option<Result<R, DomainClosed>> {
        self.try_quiesce_if_idle_impl(operation, || {})
    }

    fn try_quiesce_if_idle_impl<R>(
        &self,
        operation: impl FnOnce(DrainedGeneration) -> R,
        before_seal: impl FnOnce(),
    ) -> Option<Result<R, DomainClosed>> {
        let _transition = self.transition.try_lock()?;
        if self.closed.load(Ordering::Acquire) {
            return Some(Err(DomainClosed));
        }
        let old = self.current_generation();
        before_seal();
        if !self.generations[old.index()].try_seal_if_idle() {
            return None;
        }
        self.publish_next_locked(old);
        Some(Ok(operation(DrainedGeneration { index: old })))
    }

    fn rotate_and_run_locked<R>(
        &self,
        operation: impl FnOnce(DrainedGeneration) -> R,
    ) -> Result<R, DomainClosed> {
        if self.closed.load(Ordering::Acquire) {
            return Err(DomainClosed);
        }

        let old = self.current_generation();
        // D3: seal before publishing the replacement, so a reader that
        // loaded `old` before this transition cannot enter it afterwards.
        self.generations[old.index()].seal();
        self.publish_next_locked(old);

        // D4: the callback is entered only after all readers admitted to the
        // sealed generation have released their permits.
        self.generations[old.index()].wait_until_idle();
        Ok(operation(DrainedGeneration { index: old }))
    }

    /// Publishes the replacement after the caller has sealed `old` while
    /// retaining the transition lock. The replacement's previous callback
    /// completed before this lock was acquired, so it can be reopened safely.
    fn publish_next_locked(&self, old: GenerationIndex) {
        let next = GenerationIndex((old.index() ^ 1) as u8);
        self.generations[next.index()]
            .reopen()
            .unwrap_or_else(|_| crate::invariant::fail_stop());
        self.current.store(next.index(), Ordering::Release);
    }

    /// Permanently closes the domain and waits for both generations to drain.
    pub fn seal_and_wait(&self) {
        let _transition = self.transition.lock();
        // D5: closure is serialized with rotation, so no transition can
        // reopen a generation after the closed state becomes visible.
        self.closed.store(true, Ordering::Release);
        self.generations[0].seal_and_wait();
        self.generations[1].seal_and_wait();
    }
}

impl RotatingReadDomain<DEFAULT_STRIPE_COUNT> {
    /// Enters using the calling thread's assigned stripe.
    #[inline]
    pub fn enter_current_thread(
        &self,
    ) -> Result<RotatingReadPermit<'_, DEFAULT_STRIPE_COUNT>, DomainClosed> {
        self.enter(current_thread_stripe())
    }

    /// Enters the current generation with an owned permit using the calling
    /// thread's assigned stripe.
    ///
    /// # Safety
    ///
    /// The caller must keep this domain alive until the returned permit is
    /// dropped.
    #[inline]
    pub unsafe fn enter_owned_current_thread(
        &self,
    ) -> Result<RotatingReadOwnedPermit<DEFAULT_STRIPE_COUNT>, DomainClosed> {
        // SAFETY: delegated to the caller's obligation for the domain owner.
        unsafe { self.enter_owned(current_thread_stripe()) }
    }
}

impl<const N: usize> Default for RotatingReadDomain<N> {
    fn default() -> Self {
        Self::new()
    }
}

/// An RAII admission permit for one read generation.
pub struct RotatingReadPermit<'domain, const N: usize> {
    gate: &'domain StripedDrainGate<N>,
    stripe: usize,
}

impl<const N: usize> Drop for RotatingReadPermit<'_, N> {
    #[inline]
    fn drop(&mut self) {
        self.gate.release(self.stripe);
    }
}

/// An owned RAII admission permit for one read generation.
///
/// This is the escape hatch for owners such as `CallScope` whose type already
/// has an independent generative lifetime. The domain owner must outlive the
/// permit; the owner hierarchy, rather than shared reference counting, is what
/// establishes that invariant.
pub struct RotatingReadOwnedPermit<const N: usize> {
    gate: NonNull<StripedDrainGate<N>>,
    stripe: usize,
}

// SAFETY: the gate is a thread-safe drain counter, and the owner hierarchy
// keeps its allocation alive until this permit is dropped.
unsafe impl<const N: usize> Send for RotatingReadOwnedPermit<N> {}
// SAFETY: the permit only exposes the thread-safe release operation.
unsafe impl<const N: usize> Sync for RotatingReadOwnedPermit<N> {}

impl<const N: usize> Drop for RotatingReadOwnedPermit<N> {
    #[inline]
    fn drop(&mut self) {
        // SAFETY: `gate` points into the domain that the caller must keep
        // alive until this permit is dropped.
        unsafe { self.gate.as_ref() }.release(self.stripe);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::{Arc, Barrier, mpsc};
    use std::time::{Duration, Instant};

    #[test]
    fn starts_with_only_the_current_generation_open() {
        let domain = RotatingReadDomain::<2>::new();

        assert_eq!(domain.current_generation().index(), 0);
        assert!(!domain.generations[0].is_sealed());
        assert!(domain.generations[1].is_sealed());
    }

    #[test]
    fn rotation_seals_old_generation_before_draining_it() {
        let domain = Arc::new(RotatingReadDomain::<2>::new());
        let permit = domain.enter(0).expect("initial generation is open");
        let (finished_tx, finished_rx) = mpsc::channel();
        let rotating = Arc::clone(&domain);
        let worker = std::thread::spawn(move || {
            rotating.quiesce(|_| finished_tx.send(()).unwrap()).unwrap();
        });

        let deadline = Instant::now() + Duration::from_secs(1);
        while domain.current_generation().index() != 1 {
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
    fn owned_permit_keeps_the_generation_admitted_until_drop() {
        let domain = Arc::new(RotatingReadDomain::<2>::new());
        // SAFETY: `domain` is kept alive until `permit` is dropped below.
        let permit = unsafe { domain.enter_owned(0).expect("initial generation is open") };
        let (finished_tx, finished_rx) = mpsc::channel();
        let rotating = Arc::clone(&domain);
        let worker = std::thread::spawn(move || {
            rotating.quiesce(|_| finished_tx.send(()).unwrap()).unwrap();
        });

        let deadline = Instant::now() + Duration::from_secs(1);
        while domain.current_generation().index() != 1 {
            assert!(
                Instant::now() < deadline,
                "rotation did not publish next generation"
            );
            std::thread::yield_now();
        }
        assert!(finished_rx.try_recv().is_err());

        drop(permit);
        finished_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("owned permit must release the old generation on drop");
        worker.join().unwrap();
    }

    #[test]
    fn late_reader_cannot_enter_a_sealed_generation() {
        let domain = Arc::new(RotatingReadDomain::<2>::new());
        let (loaded_tx, loaded_rx) = mpsc::sync_channel(0);
        let (resume_tx, resume_rx) = mpsc::sync_channel(0);
        let (rotated_tx, rotated_rx) = mpsc::sync_channel(0);

        let reader_domain = Arc::clone(&domain);
        let reader = std::thread::spawn(move || {
            let first_load = Cell::new(true);
            let permit = reader_domain
                .enter_with_hook(0, |generation| {
                    if first_load.replace(false) {
                        loaded_tx.send(generation).unwrap();
                        resume_rx.recv().unwrap();
                    }
                })
                .unwrap();
            let entered_next = std::ptr::eq(permit.gate, &reader_domain.generations[1]);
            drop(permit);
            entered_next
        });

        assert_eq!(loaded_rx.recv().unwrap().index(), 0);
        let rotating_domain = Arc::clone(&domain);
        let reclaimer = std::thread::spawn(move || {
            rotating_domain.quiesce(|_| {}).unwrap();
            rotated_tx.send(()).unwrap();
        });

        rotated_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("rotation must finish before the paused reader resumes");
        assert_eq!(domain.current_generation().index(), 1);
        assert!(domain.generations[0].is_sealed());
        assert!(!domain.generations[1].is_sealed());

        resume_tx.send(()).unwrap();
        assert!(reader.join().unwrap());
        reclaimer.join().unwrap();
    }

    #[test]
    fn seal_racing_rotation_never_reopens_a_generation() {
        let domain = Arc::new(RotatingReadDomain::<2>::new());
        let start = Arc::new(Barrier::new(3));

        let rotating_domain = Arc::clone(&domain);
        let rotating_start = Arc::clone(&start);
        let rotator = std::thread::spawn(move || {
            rotating_start.wait();
            for _ in 0..16 {
                let _ = rotating_domain.quiesce(|_| {});
            }
        });

        let closing_domain = Arc::clone(&domain);
        let closing_start = Arc::clone(&start);
        let closer = std::thread::spawn(move || {
            closing_start.wait();
            closing_domain.seal_and_wait();
        });

        start.wait();
        rotator.join().unwrap();
        closer.join().unwrap();

        assert!(domain.closed.load(Ordering::Acquire));
        assert!(domain.generations[0].is_sealed());
        assert!(domain.generations[1].is_sealed());
        assert_eq!(domain.generations[0].active(), 0);
        assert_eq!(domain.generations[1].active(), 0);
        assert!(matches!(domain.enter(0), Err(DomainClosed)));
    }

    #[test]
    fn transition_callback_runs_before_the_next_transition() {
        let domain = Arc::new(RotatingReadDomain::<2>::new());
        let (started_tx, started_rx) = mpsc::sync_channel(0);
        let (release_tx, release_rx) = mpsc::sync_channel(0);
        let (second_done_tx, second_done_rx) = mpsc::sync_channel(0);

        let first_domain = Arc::clone(&domain);
        let first = std::thread::spawn(move || {
            first_domain
                .quiesce(|_| {
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                })
                .unwrap();
        });

        started_rx.recv().unwrap();
        let second_domain = Arc::clone(&domain);
        let second = std::thread::spawn(move || {
            second_domain.quiesce(|_| {}).unwrap();
            second_done_tx.send(()).unwrap();
        });

        assert!(
            second_done_rx
                .recv_timeout(Duration::from_millis(20))
                .is_err()
        );
        release_tx.send(()).unwrap();
        first.join().unwrap();
        second_done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("second transition must wait for the first callback");
        second.join().unwrap();
    }

    #[test]
    fn enter_current_thread_only_on_default_stripe_count() {
        let domain = RotatingReadDomain::<DEFAULT_STRIPE_COUNT>::new();
        let permit = domain.enter_current_thread().unwrap();
        assert_eq!(permit.stripe, current_thread_stripe());
    }

    #[test]
    fn try_quiesce_if_idle_skips_when_readers_active() {
        let domain = RotatingReadDomain::<DEFAULT_STRIPE_COUNT>::new();
        let permit = domain.enter_current_thread().unwrap();
        assert!(domain.try_quiesce_if_idle(|_| ()).is_none());
        drop(permit);
        assert!(domain.try_quiesce_if_idle(|_| ()).is_some());
    }

    #[test]
    fn miri_idle_quiesce_does_not_wait_for_a_reader_admitted_during_transition() {
        let domain = Arc::new(RotatingReadDomain::<2>::new());
        let (locked_tx, locked_rx) = mpsc::sync_channel(0);
        let (admitted_tx, admitted_rx) = mpsc::sync_channel(0);
        let (finished_tx, finished_rx) = mpsc::channel();
        let rotating = Arc::clone(&domain);
        let worker = std::thread::spawn(move || {
            let attempt = rotating.try_quiesce_if_idle_impl(
                |_| panic!("the admitted reader must prevent quiescence"),
                || {
                    locked_tx.send(()).unwrap();
                    admitted_rx.recv().unwrap();
                },
            );
            finished_tx.send(attempt.is_none()).unwrap();
        });

        locked_rx.recv().unwrap();
        // Transition ownership alone does not exclude reader admission. Use
        // a later stripe to also exercise rollback of the earlier idle one.
        let permit = domain.enter(1).unwrap();
        admitted_tx.send(()).unwrap();
        let result = finished_rx.recv_timeout(Duration::from_secs(1));
        // Always release the permit before asserting, so a regression that
        // waits for readers can finish instead of hanging the test process.
        drop(permit);
        assert!(result.expect("quiesce must not wait for the reader"));
        worker.join().unwrap();

        assert_eq!(domain.current_generation().index(), 0);
        assert!(domain.enter(0).is_ok(), "partial sealing was rolled back");
        assert_eq!(domain.try_quiesce_if_idle(|old| old.index()), Some(Ok(0)));
    }

    #[test]
    fn miri_temporal_pointer_reclamation_safety() {
        let domain = RotatingReadDomain::<DEFAULT_STRIPE_COUNT>::new();
        let val_ptr = Box::into_raw(Box::new(12345u64));
        let permit = domain.enter_current_thread().unwrap();
        // While permit is active, reading the pointer is safe
        // SAFETY: Pointer was allocated above and grace period is active.
        assert_eq!(unsafe { *val_ptr }, 12345);
        // Quiescing while permit is active is skipped
        assert!(domain.try_quiesce_if_idle(|_| ()).is_none());
        // Release the permit
        drop(permit);
        // Quiesce succeeds now that reader has drained
        let reclaimed = domain
            .try_quiesce_if_idle(|_| {
                // SAFETY: Quiesced and drained
                unsafe { Box::from_raw(val_ptr) }
            })
            .expect("should rotate")
            .expect("not closed");
        assert_eq!(*reclaimed, 12345);
    }

    #[test]
    fn miri_cross_domain_permit_mismatch_fails_to_protect_alien_pointer() {
        let domain_a = RotatingReadDomain::<DEFAULT_STRIPE_COUNT>::new();
        let domain_b = RotatingReadDomain::<DEFAULT_STRIPE_COUNT>::new();

        let val_b = Box::into_raw(Box::new(99999u64));

        // Reader acquires permit on domain_a
        let permit_a = domain_a.enter_current_thread().unwrap();

        // domain_b is completely unaware of permit_a.
        // Therefore, domain_b sees 0 readers and immediately quiesces and reclaims val_b!
        let mut reclaimed_b = None;
        let rotation = domain_b.try_quiesce_if_idle(|_| {
            // SAFETY: domain_b believes all its readers drained.
            reclaimed_b = Some(unsafe { Box::from_raw(val_b) });
        });
        assert!(
            rotation.is_some(),
            "domain_b should rotate because it has no permits"
        );
        drop(reclaimed_b); // val_b is now DEALLOCATED and RECLAIMED!

        // In contrast, if the reader holds a permit on domain_b itself:
        let val_b2 = Box::into_raw(Box::new(88888u64));
        let permit_b = domain_b.enter_current_thread().unwrap();

        // domain_b CANNOT quiesce because permit_b is active!
        let blocked_rotation = domain_b.try_quiesce_if_idle(|_| {
            unreachable!("domain_b must not reclaim while its own permit is alive");
        });
        assert!(
            blocked_rotation.is_none(),
            "domain_b quiesce must be blocked by permit_b"
        );

        // Reading val_b2 while holding permit_b is valid and protected.
        // SAFETY: permit_b is active on domain_b, blocking quiescence and reclamation.
        assert_eq!(unsafe { *val_b2 }, 88888);

        // Once permit_b is dropped, domain_b can safely quiesce and reclaim val_b2.
        drop(permit_b);
        let mut reclaimed_b2 = None;
        let successful_rotation = domain_b.try_quiesce_if_idle(|_| {
            // SAFETY: permit_b was dropped and domain_b drained.
            reclaimed_b2 = Some(unsafe { Box::from_raw(val_b2) });
        });
        assert!(successful_rotation.is_some());
        assert_eq!(*reclaimed_b2.unwrap(), 88888);

        drop(permit_a);
    }

    #[cfg(not(all(target_os = "windows", target_arch = "x86")))]
    #[cfg_attr(miri, ignore)]
    #[test]
    fn loom_generation_rotation_preserves_the_grace_period() {
        use loom::sync::Arc as LoomArc;
        use loom::sync::atomic::{AtomicBool, AtomicUsize, Ordering as LoomOrdering};
        use loom::thread as loom_thread;

        const SEALED: usize = 1;

        struct LoomGate {
            state: AtomicUsize,
        }

        impl LoomGate {
            fn new_open() -> Self {
                Self {
                    state: AtomicUsize::new(0),
                }
            }

            fn new_sealed() -> Self {
                Self {
                    state: AtomicUsize::new(SEALED),
                }
            }

            fn try_acquire(&self) -> bool {
                let mut state = self.state.load(LoomOrdering::Acquire);
                loop {
                    if state & SEALED != 0 {
                        return false;
                    }
                    match self.state.compare_exchange_weak(
                        state,
                        state + 2,
                        LoomOrdering::AcqRel,
                        LoomOrdering::Acquire,
                    ) {
                        Ok(_) => return true,
                        Err(observed) => state = observed,
                    }
                }
            }

            fn release(&self) {
                self.state.fetch_sub(2, LoomOrdering::AcqRel);
            }

            fn seal(&self) {
                self.state.fetch_or(SEALED, LoomOrdering::AcqRel);
            }

            fn try_seal_if_idle(&self) -> bool {
                self.state
                    .compare_exchange(0, SEALED, LoomOrdering::AcqRel, LoomOrdering::Acquire)
                    .is_ok()
            }

            fn reopen(&self) {
                self.state
                    .compare_exchange(SEALED, 0, LoomOrdering::AcqRel, LoomOrdering::Acquire)
                    .unwrap();
            }

            fn active(&self) -> usize {
                self.state.load(LoomOrdering::Acquire) >> 1
            }
        }

        struct LoomReadDomain {
            generations: [LoomGate; 2],
            current: AtomicUsize,
            reclaimed: [AtomicBool; 2],
        }

        impl LoomReadDomain {
            fn new() -> Self {
                Self {
                    generations: [LoomGate::new_open(), LoomGate::new_sealed()],
                    current: AtomicUsize::new(0),
                    reclaimed: [AtomicBool::new(false), AtomicBool::new(false)],
                }
            }

            fn enter(&self) -> Option<usize> {
                loop {
                    let generation = self.current.load(LoomOrdering::Acquire) & 1;
                    loom_thread::yield_now();
                    if self.generations[generation].try_acquire() {
                        return Some(generation);
                    }
                    loom_thread::yield_now();
                }
            }

            fn rotate_and_reclaim(&self, wait_for_readers: bool) {
                let old = self.current.load(LoomOrdering::Acquire) & 1;
                let next = old ^ 1;
                if wait_for_readers {
                    self.generations[old].seal();
                } else if !self.generations[old].try_seal_if_idle() {
                    return;
                }
                self.generations[next].reopen();
                self.current.store(next, LoomOrdering::Release);
                if wait_for_readers {
                    while self.generations[old].active() != 0 {
                        loom_thread::yield_now();
                    }
                } else {
                    assert_eq!(self.generations[old].active(), 0);
                }
                self.reclaimed[old].store(true, LoomOrdering::Release);
            }
        }

        // Explore both the blocking grace period and the atomic idle-seal
        // path: neither may reclaim while an admitted reader is still using
        // the generation, even when admission races the seal operation.
        for wait_for_readers in [true, false] {
            loom::model(move || {
                let domain = LoomArc::new(LoomReadDomain::new());

                let reader_domain = LoomArc::clone(&domain);
                let reader = loom_thread::spawn(move || {
                    if let Some(generation) = reader_domain.enter() {
                        assert!(
                            !reader_domain.reclaimed[generation].load(LoomOrdering::Acquire),
                            "reader observed a generation after its grace period was reclaimed"
                        );
                        reader_domain.generations[generation].release();
                    }
                });

                let reclaimer_domain = LoomArc::clone(&domain);
                let reclaimer = loom_thread::spawn(move || {
                    reclaimer_domain.rotate_and_reclaim(wait_for_readers);
                });

                reader.join().unwrap();
                reclaimer.join().unwrap();
            });
        }
    }
}
