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
use crate::sync::{Mutex, MutexGuard};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

mod protocol;
mod retirement;
mod transition;
use protocol::protocol_expr;
pub use retirement::{ClosedRetirement, RetiredQueue};

// Share the publication order with the Loom adapter. In particular, a reused
// generation must not admit a delayed reader before it becomes current.
fn publish_then_reopen(
    publish: impl FnOnce(),
    reopen: impl FnOnce(),
    between_publish_steps: impl FnOnce(),
) {
    protocol::publish_reopen!(publish(), between_publish_steps(), reopen());
}

// Keep the registration guard through publication, then release it before
// waiting for readers. The Loom model uses this same ordering helper.
fn publish_then_release_barrier<B>(barrier: B, publish: impl FnOnce(&B)) {
    protocol::publish_release!(publish(&barrier), drop(barrier));
}

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

pub use transition::{ClosedDomain, DrainedGeneration};

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
    transition: Mutex<Option<GenerationIndex>>,
    closed: AtomicBool,
}

impl<const N: usize> RotatingReadDomain<N> {
    /// Creates an open domain with generation zero selected.
    #[must_use]
    pub fn new() -> Self {
        Self {
            generations: [StripedDrainGate::new_open(), StripedDrainGate::new_sealed()],
            current: AtomicUsize::new(0),
            transition: Mutex::new(None),
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
    /// The caller must keep this domain alive through every access by the
    /// returned permit, including its final release notification. Call-scope
    /// owners satisfy that condition by being nested inside the generation
    /// owner that contains this domain. Permanent seal-and-drain synchronizes
    /// with that final access before the owner may be reclaimed.
    ///
    /// # Safety
    ///
    /// The caller must uphold the owner-lifetime requirement above.
    #[inline]
    pub unsafe fn enter_owned(
        &self,
        stripe: usize,
    ) -> Result<RotatingReadOwnedPermit<N>, DomainClosed> {
        let after_generation_load = |_: GenerationIndex| {};
        protocol::enter_reader!(generation, admission;
            self.closed.load(Ordering::Acquire), self.current_generation(),
            after_generation_load(generation),
            // SAFETY: the caller retains the issuing domain through final release.
            unsafe { self.generations[generation.index()].try_enter_detached(stripe) },
            RotatingReadOwnedPermit { _permit: admission }, DomainClosed, std::hint::spin_loop();
        )
    }

    #[inline]
    fn enter_impl(
        &self,
        stripe: usize,
        after_generation_load: impl Fn(GenerationIndex),
    ) -> Result<RotatingReadPermit<'_, N>, DomainClosed> {
        protocol::enter_reader!(generation, admission;
            self.closed.load(Ordering::Acquire), self.current_generation(),
            after_generation_load(generation),
            self.generations[generation.index()].try_enter(stripe),
            RotatingReadPermit { _permit: admission }, DomainClosed, std::hint::spin_loop();
        )
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

    /// Registers retired work while holding the currently selected queue lock.
    ///
    /// `lock` must acquire the same generation queue used by the rotation's
    /// publication barrier. Selection is rechecked under that guard; a stale
    /// selection releases its guard and retries before `register` can run.
    /// The register callback receives ownership of the guard and runs once.
    pub(crate) fn register_retired<B, R>(
        &self,
        mut lock: impl FnMut(GenerationIndex) -> B,
        register: impl FnOnce(GenerationIndex, B) -> R,
    ) -> R {
        protocol::register_retired!(generation, guard;
            self.current_generation(), lock(generation), self.current_generation(),
            drop(guard), std::hint::spin_loop(), register(generation, guard);
        )
    }
}

impl RotatingReadDomain<DEFAULT_STRIPE_COUNT> {
    /// Conservatively detects a caller that might hold a read permit.
    ///
    /// Use only before writer-side blocking maintenance. Zero in both
    /// generations excludes a permit acquired on this thread with its default
    /// stripe. It cannot detect permits transferred from another thread or
    /// acquired with an explicit different stripe. Stripe collisions can yield
    /// false positives, so this must never authorize reclamation.
    /// No reader-side bookkeeping is added.
    pub fn current_thread_may_be_reading(&self) -> bool {
        let stripe = current_thread_stripe();
        self.generations
            .iter()
            .any(|gate| gate.stripe_active(stripe) != 0)
    }

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
    /// The caller must keep this domain alive through the permit's final
    /// release notification, as described by [`Self::enter_owned`].
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

/// One admission domain and the two retirement queues it governs.
///
/// Registration, publication barriers, and certified withdrawal use these
/// same queue fields. Moving this owner moves the domain and queues together;
/// callers cannot substitute another queue array during a certified take.
pub struct RotatingRetirementDomain<const N: usize, Q> {
    domain: RotatingReadDomain<N>,
    queues: [Mutex<Q>; 2],
}

impl<const N: usize, Q: Default> Default for RotatingRetirementDomain<N, Q> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize, Q: Default> RotatingRetirementDomain<N, Q> {
    /// Creates an open domain with one empty queue for each generation.
    #[must_use]
    pub fn new() -> Self {
        Self {
            domain: RotatingReadDomain::new(),
            queues: [Mutex::new(Q::default()), Mutex::new(Q::default())],
        }
    }
}

impl<const N: usize, Q> RotatingRetirementDomain<N, Q> {
    /// Reads one queue while holding its lock without exposing a mutable guard.
    pub fn inspect_queue<R>(&self, index: usize, inspect: impl FnOnce(&Q) -> R) -> R {
        let queue = self.queues[index].lock();
        inspect(&queue)
    }

    /// Reads both queues if neither lock is busy, without exposing their guards.
    pub fn try_inspect_both<R>(&self, inspect: impl FnOnce(&Q, &Q) -> R) -> Option<R> {
        let first = self.queues[0].try_lock()?;
        let second = self.queues[1].try_lock()?;
        Some(inspect(&first, &second))
    }

    /// Registers one payload under the selected queue lock, then rechecks the
    /// selected generation before invoking the callback.
    pub fn register_retired<R>(
        &self,
        register: impl FnOnce(GenerationIndex, MutexGuard<'_, Q>) -> R,
    ) -> R {
        self.register_retired_with_hook(|_| {}, register)
    }

    /// The hook runs after each selection and before acquiring its queue.
    /// A stale selection can invoke it again; it cannot choose the queue.
    pub fn register_retired_with_hook<R>(
        &self,
        after_selection: impl Fn(GenerationIndex),
        register: impl FnOnce(GenerationIndex, MutexGuard<'_, Q>) -> R,
    ) -> R {
        self.domain.register_retired(
            |generation| {
                after_selection(generation);
                self.queues[generation.index()].lock()
            },
            register,
        )
    }

    /// Enters the currently published generation.
    #[inline]
    pub fn enter(&self, stripe: usize) -> Result<RotatingReadPermit<'_, N>, DomainClosed> {
        self.domain.enter(stripe)
    }

    /// Returns the currently published generation.
    #[must_use]
    pub fn current_generation(&self) -> GenerationIndex {
        self.domain.current_generation()
    }
}

impl<Q> RotatingRetirementDomain<DEFAULT_STRIPE_COUNT, Q> {
    /// Conservatively checks whether this thread may hold a reader permit.
    pub fn current_thread_may_be_reading(&self) -> bool {
        self.domain.current_thread_may_be_reading()
    }

    /// Enters the calling thread's assigned stripe.
    #[inline]
    pub fn enter_current_thread(
        &self,
    ) -> Result<RotatingReadPermit<'_, DEFAULT_STRIPE_COUNT>, DomainClosed> {
        self.domain.enter_current_thread()
    }

    /// Enters with an owned permit. The caller must keep this owner alive
    /// through the permit's final release notification.
    ///
    /// # Safety
    ///
    /// The caller must uphold that lifetime condition.
    #[inline]
    pub unsafe fn enter_owned_current_thread(
        &self,
    ) -> Result<RotatingReadOwnedPermit<DEFAULT_STRIPE_COUNT>, DomainClosed> {
        // SAFETY: delegated to the caller's owner-lifetime contract.
        unsafe { self.domain.enter_owned_current_thread() }
    }
}

/// An RAII admission permit for one read generation.
pub struct RotatingReadPermit<'domain, const N: usize> {
    _permit: crate::drain_gate::StripedDrainPermit<'domain, N>,
}

/// An owned RAII admission permit for one read generation.
///
/// This is the escape hatch for owners such as `CallScope` whose type already
/// has an independent generative lifetime. The domain owner must outlive the
/// permit; the owner hierarchy, rather than shared reference counting, is what
/// establishes that invariant.
pub struct RotatingReadOwnedPermit<const N: usize> {
    _permit: crate::drain_gate::DetachedStripedDrainPermit<N>,
}

#[cfg(test)]
mod tests {
    #[test]
    fn scoped_retirement_keeps_its_issuing_queue_and_terminal_owner() {
        let issuing = super::RotatingRetirementDomain::<1, usize>::new();
        let foreign = super::RotatingRetirementDomain::<1, usize>::new();
        issuing.register_retired(|generation, mut queue| {
            assert_eq!(generation.index(), 0);
            *queue = 7;
        });
        issuing
            .quiesce(|retired| {
                assert_eq!(retired.generation().index(), 0);
                assert!(retired.belongs_to(&issuing));
                assert!(!retired.belongs_to(&foreign));
                assert_eq!(
                    retired.take(|mut queue| std::mem::replace(&mut *queue, 70)),
                    7
                );
            })
            .unwrap();
        assert_eq!(issuing.inspect_queue(0, |queue| *queue), 70);
        issuing.register_retired(|_, mut queue| *queue = 9);
        issuing
            .quiesce(|retired| {
                assert_eq!(retired.generation().index(), 1);
                assert_eq!(
                    retired.take(|mut queue| std::mem::replace(&mut *queue, 90)),
                    9
                );
            })
            .unwrap();
        let closed = issuing.seal_and_wait();
        assert!(closed.belongs_to(&issuing));
        assert!(!closed.belongs_to(&foreign));
        assert_eq!(
            closed.take_queues(|first, second| [*first, *second]),
            [70, 90]
        );
        // Re-observation of permanent closure remains valid. Emptying a queue
        // and a repeated observation do not duplicate any payload ownership.
        closed.take_queues(|mut first, mut second| {
            *first = 0;
            *second = 0;
        });
        assert_eq!(
            closed.take_queues(|first, second| [*first, *second]),
            [0, 0]
        );
    }

    use super::*;
    use std::cell::Cell;
    use std::sync::{Arc, Barrier, mpsc};
    use std::time::{Duration, Instant};

    #[test]
    fn miri_polled_grace_completes_despite_overlapping_later_readers() {
        let domain = RotatingReadDomain::<2>::new();
        let old = domain.enter(0).unwrap();
        assert!(
            domain
                .poll_quiesce_with_publication_barrier(|_| Some(()), |_| ())
                .is_none()
        );
        assert_eq!(domain.current_generation().index(), 1);
        let later = domain.enter(1).unwrap();
        drop(old);
        assert_eq!(
            domain.poll_quiesce_with_publication_barrier(
                |_| Some(()),
                |old| old.index_for(&domain).unwrap()
            ),
            Some(Ok(0))
        );
        assert!(
            domain
                .poll_quiesce_with_publication_barrier(|_| Some(()), |_| ())
                .is_none()
        );
        assert_eq!(domain.current_generation().index(), 0);
        let newest = domain.enter(0).unwrap();
        drop(later);
        assert_eq!(
            domain.poll_quiesce_with_publication_barrier(
                |_| Some(()),
                |old| old.index_for(&domain).unwrap()
            ),
            Some(Ok(1))
        );
        drop(newest);
    }

    #[test]
    fn miri_blocking_quiescence_preserves_pending_and_current_batches() {
        let domain = RotatingReadDomain::<2>::new();
        let old = domain.enter(0).unwrap();
        assert!(
            domain
                .poll_quiesce_with_publication_barrier(|_| Some(()), |_| ())
                .is_none()
        );
        drop(old);
        assert_eq!(
            domain
                .quiesce(|old| old.index_for(&domain).unwrap())
                .unwrap(),
            [Some(0), Some(1)]
        );
        assert_eq!(domain.current_generation().index(), 0);
        assert!(domain.transition.lock().is_none());
    }

    #[test]
    fn miri_idle_attempt_finishes_pending_before_reusing_a_generation() {
        let domain = RotatingReadDomain::<2>::new();
        let old = domain.enter(0).unwrap();
        assert!(
            domain
                .poll_quiesce_with_publication_barrier(|_| Some(()), |_| ())
                .is_none()
        );
        let later = domain.enter(1).unwrap();
        assert!(domain.try_quiesce_if_idle(|_| ()).is_none());
        drop(old);
        assert_eq!(
            domain.try_quiesce_if_idle(|old| old.index_for(&domain).unwrap()),
            Some(Ok(0))
        );
        assert_eq!(domain.current_generation().index(), 1);
        assert!(domain.try_quiesce_if_idle(|_| ()).is_none());
        drop(later);
        assert_eq!(
            domain.try_quiesce_if_idle(|old| old.index_for(&domain).unwrap()),
            Some(Ok(1))
        );
    }

    #[test]
    fn miri_callback_panic_keeps_generation_pending_across_apis() {
        use std::panic::{AssertUnwindSafe, catch_unwind};
        for idle in [false, true] {
            let domain = RotatingReadDomain::<2>::new();
            let result = catch_unwind(AssertUnwindSafe(|| {
                if idle {
                    domain.try_quiesce_if_idle::<()>(|_| panic!("injected callback failure"));
                } else {
                    domain
                        .quiesce::<()>(|_| panic!("injected callback failure"))
                        .unwrap();
                }
            }));
            assert!(result.is_err());
            assert_eq!(domain.current_generation().index(), 1);
            assert_eq!(
                domain.poll_quiesce_with_publication_barrier(
                    |_| Some(()),
                    |old| old.index_for(&domain).unwrap()
                ),
                Some(Ok(0))
            );
            assert_eq!(domain.current_generation().index(), 1);
        }
    }

    #[test]
    fn miri_first_batch_drops_unlocked_if_second_quiescence_stage_panics() {
        use std::panic::{AssertUnwindSafe, catch_unwind};
        struct Batch<'a> {
            domain: &'a RotatingReadDomain<2>,
            dropped: &'a Cell<bool>,
        }
        impl Drop for Batch<'_> {
            fn drop(&mut self) {
                assert!(self.domain.transition.try_lock().is_some());
                self.dropped.set(true);
            }
        }
        for barrier_panics in [false, true] {
            let domain = RotatingReadDomain::<2>::new();
            let old = domain.enter(0).unwrap();
            assert!(
                domain
                    .poll_quiesce_with_publication_barrier(|_| Some(()), |_| ())
                    .is_none()
            );
            drop(old);
            let dropped = Cell::new(false);
            let result = catch_unwind(AssertUnwindSafe(|| {
                let _ = domain.quiesce_with_publication_barrier(
                    |_| {
                        assert!(!barrier_panics, "injected second barrier failure");
                    },
                    |old| {
                        assert_eq!(
                            old.index_for(&domain),
                            Some(0),
                            "injected second callback failure"
                        );
                        Batch {
                            domain: &domain,
                            dropped: &dropped,
                        }
                    },
                );
            }));
            assert!(result.is_err());
            assert!(dropped.get());
        }
    }

    #[test]
    fn miri_seal_drains_both_sides_of_a_pending_rotation() {
        let domain = RotatingReadDomain::<2>::new();
        let old = domain.enter(0).unwrap();
        assert!(
            domain
                .poll_quiesce_with_publication_barrier(|_| Some(()), |_| ())
                .is_none()
        );
        let current = domain.enter(1).unwrap();
        std::thread::scope(|scope| {
            let sealing = scope.spawn(|| domain.seal_and_wait());
            while !domain.closed.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            drop(old);
            drop(current);
            sealing.join().unwrap();
        });
        assert_eq!(
            domain.poll_quiesce_with_publication_barrier(|_| Some(()), |_| ()),
            Some(Err(DomainClosed))
        );
    }

    #[test]
    fn publication_barrier_is_released_before_old_readers_drain() {
        let domain = Arc::new(RotatingReadDomain::<2>::new());
        let queue = Arc::new(Mutex::new(()));
        let permit = domain.enter(0).unwrap();
        let rotating = Arc::clone(&domain);
        let registration = Arc::clone(&queue);
        let (finished_tx, finished_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            rotating
                .quiesce_with_publication_barrier(
                    |_| registration.lock(),
                    |_| {
                        assert!(registration.try_lock().is_some());
                        finished_tx.send(()).unwrap();
                    },
                )
                .unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if domain.current_generation().index() == 1 && queue.try_lock().is_some() {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(finished_rx.try_recv().is_err());
        // Registration remains available while the old reader is held.
        drop(queue.lock());
        drop(permit);
        finished_rx.recv_timeout(Duration::from_secs(30)).unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn idle_publication_barrier_never_waits_for_registration_or_readers() {
        let domain = RotatingReadDomain::<2>::new();
        let queue = Mutex::new(());
        let registration = queue.lock();
        assert!(
            domain
                .try_quiesce_if_idle_with_publication_barrier(
                    |_| queue.try_lock(),
                    |_| panic!("busy registration must skip rotation"),
                )
                .is_none()
        );
        drop(registration);
        let reader = domain.enter(0).unwrap();
        assert!(
            domain
                .try_quiesce_if_idle_with_publication_barrier(
                    |_| queue.try_lock(),
                    |_| panic!("busy reader must skip rotation"),
                )
                .is_none()
        );
        assert!(queue.try_lock().is_some());
        assert_eq!(domain.current_generation().index(), 0);
        drop(reader);
        domain
            .try_quiesce_if_idle_with_publication_barrier(
                |_| queue.try_lock(),
                |_| assert!(queue.try_lock().is_some()),
            )
            .unwrap()
            .unwrap();
    }

    // This model isolates the publication/registration handoff. Reader gate
    // admission and generation reuse are covered by the separate domain model.
    #[cfg(not(all(target_os = "windows", target_arch = "x86")))]
    fn model_deferred_registration(synchronize_publication: bool) {
        use loom::sync::atomic::{AtomicBool as LoomBool, AtomicUsize as LoomUsize, Ordering as O};
        use loom::sync::{Arc as LoomArc, Mutex as LoomMutex};
        struct State {
            published: LoomBool,
            current: LoomUsize,
            retired_in_old: LoomMutex<bool>,
            record: loom::cell::UnsafeCell<usize>,
        }
        loom::model(move || {
            let state = LoomArc::new(State {
                published: LoomBool::new(true),
                current: LoomUsize::new(0),
                retired_in_old: LoomMutex::new(false),
                record: loom::cell::UnsafeCell::new(42),
            });
            let writer_state = LoomArc::clone(&state);
            let writer = loom::thread::spawn(move || {
                writer_state.published.store(false, O::Release);
                let mut queue = writer_state.retired_in_old.lock().unwrap();
                if writer_state.current.load(O::Acquire) == 0 {
                    *queue = true;
                }
            });
            let rotating_state = LoomArc::clone(&state);
            let rotating = loom::thread::spawn(move || {
                let barrier =
                    synchronize_publication.then(|| rotating_state.retired_in_old.lock().unwrap());
                publish_then_release_barrier(barrier, |_| {
                    rotating_state.current.store(1, O::Release)
                });
                if *rotating_state.retired_in_old.lock().unwrap() {
                    // SAFETY: the positive model proves a new reader cannot
                    // observe this withdrawn old-generation record. The
                    // negative model intentionally violates that protocol.
                    rotating_state
                        .record
                        .with_mut(|value| unsafe { value.write(0) });
                }
            });
            let reader_state = LoomArc::clone(&state);
            let reader = loom::thread::spawn(move || {
                if reader_state.current.load(O::Acquire) == 1
                    && reader_state.published.load(O::Acquire)
                {
                    // SAFETY: Loom diagnoses a stale-pointer access without
                    // the publication barrier; no physical pointer is freed.
                    reader_state.record.with(|value| unsafe {
                        std::hint::black_box(value.read());
                    });
                }
            });
            writer.join().unwrap();
            rotating.join().unwrap();
            reader.join().unwrap();
        });
    }

    #[cfg(not(all(target_os = "windows", target_arch = "x86")))]
    #[cfg_attr(miri, ignore)]
    #[test]
    fn loom_publication_barrier_orders_deferred_withdrawal_before_new_readers() {
        model_deferred_registration(true);
    }

    #[cfg(not(all(target_os = "windows", target_arch = "x86")))]
    #[cfg_attr(miri, ignore)]
    #[test]
    #[should_panic(expected = "Causality violation")]
    fn loom_unbarred_publication_can_expose_a_reclaimed_record_to_new_readers() {
        model_deferred_registration(false);
    }

    #[test]
    fn caller_stripe_observation_includes_a_draining_generation() {
        let domain = Arc::new(RotatingReadDomain::<DEFAULT_STRIPE_COUNT>::new());
        assert!(!domain.current_thread_may_be_reading());
        let permit = domain.enter_current_thread().unwrap();
        assert!(domain.current_thread_may_be_reading());
        let old = domain.current_generation();
        let rotating = Arc::clone(&domain);
        let worker = std::thread::spawn(move || rotating.quiesce(|_| ()).unwrap());
        let deadline = Instant::now() + Duration::from_secs(30);
        while domain.current_generation() == old {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(domain.current_thread_may_be_reading());
        drop(permit);
        worker.join().unwrap();
        assert!(!domain.current_thread_may_be_reading());
        // A different thread using the same stripe is conservatively included.
        let stripe = current_thread_stripe();
        std::thread::scope(|threads| {
            let (ready_tx, ready_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let domain = &domain;
            threads.spawn(move || {
                let _permit = domain.enter(stripe).unwrap();
                ready_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            });
            ready_rx.recv().unwrap();
            assert!(domain.current_thread_may_be_reading());
            release_tx.send(()).unwrap();
        });
        assert!(!domain.current_thread_may_be_reading());
    }

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
        while domain.current_generation().index() != 1 || domain.generations[1].is_sealed() {
            assert!(
                Instant::now() < deadline,
                "rotation did not publish and reopen next generation"
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
        while domain.current_generation().index() != 1 || domain.generations[1].is_sealed() {
            assert!(
                Instant::now() < deadline,
                "rotation did not publish and reopen next generation"
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
    fn miri_owned_permit_allows_immediate_domain_reclamation_after_drain() {
        for _ in 0..8 {
            let owner = crate::published_owner::PublishedOwner::new(RotatingReadDomain::<2>::new());
            // SAFETY: sealing and draining retains the allocation until the
            // permit's final atomic/notification field access is complete.
            let permit = unsafe { owner.enter_owned(0) }.unwrap();
            let worker = std::thread::spawn(move || drop(permit));
            owner.seal_and_wait();
            drop(owner);
            worker.join().unwrap();
        }
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
            let entered_next = reader_domain.generations[1].stripe_active(0) == 1
                && reader_domain.generations[0].stripe_active(0) == 0;
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
    fn miri_delayed_reader_cannot_enter_reused_generation_before_publication() {
        #[derive(Debug, Eq, PartialEq)]
        enum ReaderEvent {
            LoadedInitial,
            Retried,
            Admitted,
        }

        let domain = Arc::new(RotatingReadDomain::<2>::new());
        let (events_tx, events_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::sync_channel(0);
        let reader_domain = Arc::clone(&domain);
        let reader = std::thread::spawn(move || {
            let attempts = Cell::new(0);
            let permit = reader_domain
                .enter_with_hook(0, |generation| {
                    let attempt = attempts.replace(attempts.get() + 1);
                    if attempt == 0 {
                        assert_eq!(generation.index(), 0);
                        events_tx.send(ReaderEvent::LoadedInitial).unwrap();
                        resume_rx.recv().unwrap();
                    } else if attempt == 1 {
                        events_tx.send(ReaderEvent::Retried).unwrap();
                    }
                })
                .unwrap();
            events_tx.send(ReaderEvent::Admitted).unwrap();
            drop(permit);
        });

        assert_eq!(events_rx.recv().unwrap(), ReaderEvent::LoadedInitial);
        domain.quiesce(|_| {}).unwrap();

        // Hold the second rotation between publication and gate reopening.
        // The reader still carries zero from before the first rotation and
        // must retry while the reused gate is sealed. Reopening first would
        // admit it before zero is published, so retirement in generation one
        // could reclaim a pointer without waiting for this reader.
        let event = domain.test_publish_reopen_window(|| {
            resume_tx.send(()).unwrap();
            events_rx.recv_timeout(Duration::from_secs(1)).unwrap()
        });

        // Join before asserting so the regression cannot strand a reader.
        reader.join().unwrap();
        assert_eq!(event, ReaderEvent::Retried);
        assert_eq!(events_rx.recv().unwrap(), ReaderEvent::Admitted);
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
        let stripe = current_thread_stripe();
        assert_eq!(domain.generations[0].stripe_active(stripe), 1);
        drop(permit);
        assert_eq!(domain.generations[0].stripe_active(stripe), 0);
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
        assert_eq!(
            domain.try_quiesce_if_idle(|old| old.index_for(&domain).unwrap()),
            Some(Ok(0))
        );
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

        use crate::sealable_counter::loom_support::Counter as LoomGate;

        struct LoomReadDomain {
            generations: [LoomGate; 2],
            current: AtomicUsize,
            reclaimed: [AtomicBool; 2],
        }

        impl LoomReadDomain {
            fn new() -> Self {
                Self {
                    generations: [LoomGate::new(), LoomGate::new_sealed()],
                    current: AtomicUsize::new(0),
                    reclaimed: [AtomicBool::new(false), AtomicBool::new(false)],
                }
            }

            fn enter(&self) -> Option<usize> {
                self.enter_result().ok()
            }

            fn enter_result(&self) -> Result<usize, ()> {
                protocol::enter_reader!(generation, _admission;
                    false, self.current.load(LoomOrdering::Acquire) & 1,
                    loom_thread::yield_now(), self.generations[generation].try_acquire(),
                    generation, (), loom_thread::yield_now();
                )
            }

            fn rotate_and_reclaim(&self, wait_for_readers: bool) {
                let old = self.current.load(LoomOrdering::Acquire) & 1;
                let next = old ^ 1;
                if wait_for_readers {
                    self.generations[old].seal();
                } else if !self.generations[old].try_seal_if_idle() {
                    return;
                }
                publish_then_reopen(
                    || self.current.store(next, LoomOrdering::Release),
                    || self.generations[next].reopen().unwrap(),
                    || {},
                );
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

        // Two rotations can reuse a generation selected by a delayed reader.
        // Admission may straddle a later seal, but an admitted generation
        // that is no longer current must already be sealed. An open gate in
        // a noncurrent generation is the unsafe publication window.
        for wait_for_readers in [true, false] {
            loom::model(move || {
                let domain = LoomArc::new(LoomReadDomain::new());
                let reader_domain = LoomArc::clone(&domain);
                let reader = loom_thread::spawn(move || {
                    if let Some(generation) = reader_domain.enter() {
                        if reader_domain.current.load(LoomOrdering::Acquire) != generation {
                            assert!(
                                reader_domain.generations[generation].is_sealed(),
                                "reader entered a reused generation before publication"
                            );
                        }
                        reader_domain.generations[generation].release();
                    }
                });
                let reclaimer_domain = LoomArc::clone(&domain);
                let reclaimer = loom_thread::spawn(move || {
                    reclaimer_domain.rotate_and_reclaim(wait_for_readers);
                    reclaimer_domain.rotate_and_reclaim(wait_for_readers);
                });
                reader.join().unwrap();
                reclaimer.join().unwrap();
            });
        }
    }
}
