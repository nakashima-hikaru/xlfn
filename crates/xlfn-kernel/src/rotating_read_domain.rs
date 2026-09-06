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

use crate::drain_gate::{StripedDrainGate, current_thread_stripe};
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
/// While the domain is open, exactly one generation is open. A transition
/// seals the current generation before publishing the replacement, waits for
/// the sealed generation to become idle, and invokes its callback while the
/// transition lock remains held.
///
/// The protocol maintains these invariants:
///
/// - D1: exactly one generation is open while the domain is not closed.
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

    /// Enters using the calling thread's assigned stripe.
    #[inline]
    pub fn enter_current_thread(&self) -> Result<RotatingReadPermit<'_, N>, DomainClosed> {
        self.enter(current_thread_stripe())
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
    ) -> Result<RotatingReadOwnedPermit<N>, DomainClosed> {
        // SAFETY: delegated to the caller's obligation for the domain owner.
        unsafe { self.enter_owned(current_thread_stripe()) }
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
    /// `None` means the transition lock was busy or the domain was already
    /// closed. The nested result reports the same closed condition when the
    /// lock was acquired before closure was observed.
    pub fn try_quiesce<R>(
        &self,
        operation: impl FnOnce(DrainedGeneration) -> R,
    ) -> Option<Result<R, DomainClosed>> {
        let _transition = self.transition.try_lock()?;
        Some(self.rotate_and_run_locked(operation))
    }

    fn rotate_and_run_locked<R>(
        &self,
        operation: impl FnOnce(DrainedGeneration) -> R,
    ) -> Result<R, DomainClosed> {
        if self.closed.load(Ordering::Acquire) {
            return Err(DomainClosed);
        }

        let old = self.current_generation();
        let next = GenerationIndex((old.index() ^ 1) as u8);

        // D3: seal before publishing the replacement, so a reader that
        // loaded `old` before this transition cannot enter it afterwards.
        self.generations[old.index()].seal();
        self.generations[next.index()]
            .reopen()
            .unwrap_or_else(|_| crate::invariant::fail_stop());
        self.current.store(next.index(), Ordering::Release);

        // D4: the callback is entered only after all readers admitted to the
        // sealed generation have released their permits.
        self.generations[old.index()].wait_until_idle();
        Ok(operation(DrainedGeneration { index: old }))
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

    #[cfg(not(all(target_os = "windows", target_arch = "x86")))]
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

            fn rotate_and_reclaim(&self) {
                let old = self.current.load(LoomOrdering::Acquire) & 1;
                let next = old ^ 1;
                self.generations[old].seal();
                self.generations[next].reopen();
                self.current.store(next, LoomOrdering::Release);
                while self.generations[old].active() != 0 {
                    loom_thread::yield_now();
                }
                self.reclaimed[old].store(true, LoomOrdering::Release);
            }
        }

        loom::model(|| {
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
                reclaimer_domain.rotate_and_reclaim();
            });

            reader.join().unwrap();
            reclaimer.join().unwrap();
        });
    }
}
