//! Single- and multi-stripe sealable admission gates.

use std::cell::Cell;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering};

use crossbeam_utils::CachePadded;
use parking_lot::{Condvar, Mutex};

use crate::sealable_counter::{ReleaseOutcome, ReopenError, SealableCounter, Sealed};

/// Synchronization surface shared by the production protocol and its Loom
/// model. Implementations are private and cannot run application callbacks.
trait IdleWait {
    type Guard<'a>
    where
        Self: 'a;

    fn lock(&self) -> Self::Guard<'_>;
    fn wait<'a>(&'a self, guard: Self::Guard<'a>) -> Self::Guard<'a>;
    fn notify_all(&self);
}

struct IdleNotification {
    lock: Mutex<()>,
    changed: Condvar,
}

impl IdleNotification {
    const fn new() -> Self {
        Self {
            lock: Mutex::new(()),
            changed: Condvar::new(),
        }
    }
}

impl IdleWait for IdleNotification {
    type Guard<'a> = parking_lot::MutexGuard<'a, ()>;

    fn lock(&self) -> Self::Guard<'_> {
        self.lock.lock()
    }

    fn wait<'a>(&'a self, mut guard: Self::Guard<'a>) -> Self::Guard<'a> {
        self.changed.wait(&mut guard);
        guard
    }

    fn notify_all(&self) {
        self.changed.notify_all();
    }
}

/// A stack-local view for releases that can permit immediate owner reclamation.
/// Each borrowed field is interior mutable; the view avoids keeping a shared
/// borrow of the containing gate's immutable padding alive past mutex unlock.
struct IdleNotificationRef<'a> {
    lock: &'a Mutex<()>,
    changed: &'a Condvar,
}

impl IdleWait for IdleNotificationRef<'_> {
    type Guard<'a>
        = parking_lot::MutexGuard<'a, ()>
    where
        Self: 'a;

    fn lock(&self) -> Self::Guard<'_> {
        self.lock.lock()
    }

    fn wait<'a>(&'a self, mut guard: Self::Guard<'a>) -> Self::Guard<'a> {
        self.changed.wait(&mut guard);
        guard
    }

    fn notify_all(&self) {
        self.changed.notify_all();
    }
}

fn release_and_notify<W: IdleWait>(
    idle: &W,
    release: impl FnOnce() -> ReleaseOutcome,
) -> ReleaseOutcome {
    // The last count remains live until this lock is acquired. Publishing
    // zero before locking would let a waiter reclaim the gate while release
    // was still trying to access its mutex/condvar.
    let _guard = idle.lock();
    let outcome = release();
    if outcome == ReleaseOutcome::BecameIdle {
        idle.notify_all();
    }
    outcome
}

fn wait_for_idle<W: IdleWait>(idle: &W, mut register_and_observe: impl FnMut() -> usize) {
    let mut guard = idle.lock();
    // Register through each counter's RMW under this mutex. Either the last
    // release won that RMW and we observe zero, or it must retain its count
    // until we atomically unlock and park. Re-register after waking because
    // a serialized reopen may have started a new generation in the meantime.
    while register_and_observe() != 0 {
        guard = idle.wait(guard);
    }
}

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
    idle: IdleNotification,
}

impl DrainGate {
    pub const fn new_open() -> Self {
        Self {
            counter: SealableCounter::new_open(),
            idle: IdleNotification::new(),
        }
    }

    pub const fn new_sealed() -> Self {
        Self {
            counter: SealableCounter::new_sealed(),
            idle: IdleNotification::new(),
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
        let counter = &self.counter;
        if let Some(outcome) = counter.try_release_without_notification() {
            // No gate access is allowed after this final-count CAS: an owner
            // may already observe zero and reclaim the gate.
            return outcome;
        }
        release_and_notify(&self.idle, || counter.release())
    }

    /// Releases a raw capability without borrowing the whole allocation
    /// across the final count/notification operation.
    ///
    /// # Safety
    /// The pointer must identify a live gate with one count owned by this
    /// caller. Its owner may reclaim only after sealing and waiting for idle.
    pub(crate) unsafe fn release_owned(gate: NonNull<Self>) -> ReleaseOutcome {
        let gate = gate.as_ptr();
        // SAFETY: the caller's active count retains every field until this
        // release publishes zero. Only interior-mutable fields are borrowed.
        let counter = unsafe { &(*gate).counter };
        if let Some(outcome) = counter.try_release_without_notification() {
            return outcome;
        }
        // SAFETY: the slow path still owns its last count. Do not create a
        // reference to DrainGate or IdleNotification spanning the unlock.
        let lock = unsafe { &(*gate).idle.lock };
        // SAFETY: the same live count retains the condition variable.
        let changed = unsafe { &(*gate).idle.changed };
        let idle = IdleNotificationRef { lock, changed };
        release_and_notify(&idle, || counter.release())
    }

    #[inline]
    pub fn seal(&self) {
        self.counter.seal();
    }

    /// Waits for an idle observation. Seal admission first when that
    /// observation must remain valid for reclamation or shutdown.
    pub fn wait_until_idle(&self) {
        wait_for_idle(&self.idle, || self.counter.mark_waiting());
    }

    pub fn seal_and_wait(&self) {
        self.seal();
        self.wait_until_idle();
    }

    #[inline]
    pub fn reopen(&self) -> Result<(), ReopenError> {
        let _guard = self.idle.lock();
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
    idle: IdleNotification,
}

impl<const N: usize> StripedDrainGate<N> {
    pub const fn new_open() -> Self {
        Self {
            counters: [const { CachePadded::new(SealableCounter::new_open()) }; N],
            idle: IdleNotification::new(),
        }
    }

    pub const fn new_sealed() -> Self {
        Self {
            counters: [const { CachePadded::new(SealableCounter::new_sealed()) }; N],
            idle: IdleNotification::new(),
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
        if let Some(outcome) = counter.try_release_without_notification() {
            // Do not inspect another stripe or notification state after
            // releasing the last count that might keep this gate alive.
            return outcome;
        }
        // Any stripe becoming idle may be the last one. Notify under the
        // shared mutex; the waiter rechecks every stripe before returning.
        release_and_notify(&self.idle, || counter.release())
    }

    /// Raw-capability counterpart of [`Self::release`].
    ///
    /// # Safety
    /// The caller must own one count on `stripe` in this live gate. The owner
    /// may reclaim the gate only after sealing and waiting for all stripes.
    pub(crate) unsafe fn release_owned(gate: NonNull<Self>, stripe: usize) -> ReleaseOutcome {
        let gate = gate.as_ptr();
        // SAFETY: the active count retains the allocation. Deref projects
        // through CachePadded before release; no borrow of its padding is
        // passed to the notification tail.
        let counter: &SealableCounter = unsafe { &(*gate).counters[stripe] };
        if let Some(outcome) = counter.try_release_without_notification() {
            return outcome;
        }
        // SAFETY: the slow path retains its final count until locking.
        let lock = unsafe { &(*gate).idle.lock };
        // SAFETY: the same live count retains the condition variable.
        let changed = unsafe { &(*gate).idle.changed };
        let idle = IdleNotificationRef { lock, changed };
        release_and_notify(&idle, || counter.release())
    }

    pub fn seal(&self) {
        for counter in &self.counters {
            counter.seal();
        }
    }

    /// Seals every stripe only if each is idle, without waiting for readers.
    /// The caller must serialize this operation with other seal/reopen calls.
    /// On failure, stripes sealed by this attempt are reopened before return.
    pub(crate) fn try_seal_if_idle(&self) -> bool {
        for (index, counter) in self.counters.iter().enumerate() {
            if !counter.try_seal_if_idle() {
                // Successfully sealed stripes cannot admit readers. They are
                // still idle, so rollback never needs to wait for a drain.
                for sealed in &self.counters[..index] {
                    sealed
                        .undo_idle_seal()
                        .unwrap_or_else(|_| crate::invariant::fail_stop());
                }
                return false;
            }
        }
        true
    }

    /// Waits until every stripe is observed idle. Seal all stripes first when
    /// an owner needs a stable grace period rather than an open-gate snapshot.
    pub fn wait_until_idle(&self) {
        wait_for_idle(&self.idle, || {
            self.counters.iter().fold(0_usize, |active, counter| {
                active
                    .checked_add(counter.mark_waiting())
                    .unwrap_or_else(|| crate::invariant::fail_stop())
            })
        });
    }

    pub fn seal_and_wait(&self) {
        self.seal();
        self.wait_until_idle();
    }

    pub fn reopen(&self) -> Result<(), ReopenError> {
        let _guard = self.idle.lock();
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

    /// Writer-side observation only. A nonzero count may belong to any
    /// thread assigned this stripe; it is not proof of thread ownership.
    pub(crate) fn stripe_active(&self, stripe: usize) -> usize {
        self.counter(stripe).active()
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

    #[test]
    fn miri_idle_seal_rolls_back_partial_progress_without_waiting() {
        let gate = StripedDrainGate::<3>::new_open();
        let permit = gate.try_enter(1).unwrap();

        assert!(!gate.try_seal_if_idle());
        assert!(gate.counters.iter().all(|counter| !counter.is_sealed()));
        let first = gate.try_enter(0).expect("rolled-back stripe is open");
        let last = gate.try_enter(2).expect("unvisited stripe stays open");
        drop((first, last, permit));

        assert!(gate.try_seal_if_idle());
        assert!(gate.is_sealed());
        assert_eq!(gate.active(), 0);
        assert!(gate.try_enter(0).is_err());
        gate.reopen().unwrap();
        assert!(gate.try_enter(0).is_ok());
    }

    #[test]
    fn miri_wait_does_not_return_before_final_release_finishes_using_the_gate() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::mpsc;

        let gate = Arc::new(DrainGate::new_open());
        gate.try_acquire().unwrap();
        gate.seal();
        let (zero_tx, zero_rx) = mpsc::channel();
        let (finish_tx, finish_rx) = mpsc::channel();
        let (drained_tx, drained_rx) = mpsc::channel();
        let released = Arc::new(AtomicBool::new(false));
        let releasing_gate = Arc::clone(&gate);
        let releasing_done = Arc::clone(&released);
        let releaser = std::thread::spawn(move || {
            // Exercise the production slow-release protocol with a hook after
            // zero is published, while the notification mutex is still held.
            release_and_notify(&releasing_gate.idle, || {
                let outcome = releasing_gate.counter.release();
                zero_tx.send(()).unwrap();
                finish_rx.recv().unwrap();
                releasing_done.store(true, Ordering::Release);
                outcome
            });
        });
        zero_rx.recv().unwrap();
        let waiting_gate = Arc::clone(&gate);
        let waiter = std::thread::spawn(move || {
            waiting_gate.wait_until_idle();
            assert!(released.load(Ordering::Acquire));
            drained_tx.send(()).unwrap();
        });
        assert!(drained_rx.recv_timeout(Duration::from_millis(10)).is_err());
        finish_tx.send(()).unwrap();
        drained_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        releaser.join().unwrap();
        waiter.join().unwrap();
    }
}

#[cfg(all(test, not(all(target_os = "windows", target_arch = "x86"))))]
mod loom_tests {
    use super::{IdleWait, ReleaseOutcome, release_and_notify, wait_for_idle};
    use crate::sealable_counter::loom_support::Counter;
    use loom::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use loom::sync::{Arc, Condvar, Mutex, MutexGuard};
    use loom::thread;

    struct ModelNotification {
        lock: Mutex<()>,
        changed: Condvar,
        reclaimed: AtomicBool,
    }

    impl IdleWait for ModelNotification {
        type Guard<'a> = MutexGuard<'a, ()>;

        fn lock(&self) -> Self::Guard<'_> {
            self.lock.lock().unwrap()
        }

        fn wait<'a>(&'a self, guard: Self::Guard<'a>) -> Self::Guard<'a> {
            self.changed.wait(guard).unwrap()
        }

        fn notify_all(&self) {
            assert!(
                !self.reclaimed.load(Ordering::Acquire),
                "release accessed the gate after drain allowed reclamation"
            );
            self.changed.notify_all();
        }
    }

    struct ModelGate<const N: usize> {
        counters: [Counter; N],
        idle: ModelNotification,
    }

    impl<const N: usize> ModelGate<N> {
        fn new() -> Self {
            Self {
                counters: std::array::from_fn(|_| Counter::new()),
                idle: ModelNotification {
                    lock: Mutex::new(()),
                    changed: Condvar::new(),
                    reclaimed: AtomicBool::new(false),
                },
            }
        }

        fn acquire(&self, stripe: usize) {
            self.counters[stripe].try_acquire().unwrap();
        }

        fn release(&self, stripe: usize) -> ReleaseOutcome {
            let counter = &self.counters[stripe];
            if let Some(outcome) = counter.try_release_without_notification() {
                return outcome;
            }
            release_and_notify(&self.idle, || counter.release())
        }

        fn wait(&self) {
            wait_for_idle(&self.idle, || {
                self.counters.iter().map(Counter::mark_waiting).sum()
            });
        }

        fn seal(&self) {
            for counter in &self.counters {
                counter.seal();
            }
        }

        fn reopen(&self) {
            let _guard = self.idle.lock();
            for counter in &self.counters {
                counter.reopen().unwrap();
            }
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn loom_waiter_racing_final_release_cannot_miss_notification_or_release_tail() {
        for sealed in [false, true] {
            loom::model(move || {
                let gate = Arc::new(ModelGate::<1>::new());
                gate.acquire(0);
                if sealed {
                    gate.seal();
                }
                let value = Arc::new(AtomicUsize::new(0));
                let releasing_gate = Arc::clone(&gate);
                let releasing_value = Arc::clone(&value);
                let releaser = thread::spawn(move || {
                    releasing_value.store(7, Ordering::Relaxed);
                    releasing_gate.release(0);
                });

                gate.wait();
                assert_eq!(value.load(Ordering::Relaxed), 7);
                gate.idle.reclaimed.store(true, Ordering::Release);
                releaser.join().unwrap();
            });
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn loom_striped_wait_observes_every_release_and_eventually_finishes() {
        let mut model = loom::model::Builder::new();
        // Bound this larger three-thread/two-counter model to two preemptions.
        // The one-counter release/registration model above is exhaustive.
        model.preemption_bound = Some(2);
        model.check(|| {
            let gate = Arc::new(ModelGate::<2>::new());
            let values = Arc::new([AtomicUsize::new(0), AtomicUsize::new(0)]);
            gate.acquire(0);
            gate.acquire(1);
            gate.seal();
            let mut releases = Vec::new();
            for stripe in 0..2 {
                let releasing_gate = Arc::clone(&gate);
                let releasing_values = Arc::clone(&values);
                releases.push(thread::spawn(move || {
                    releasing_values[stripe].store(stripe + 1, Ordering::Relaxed);
                    releasing_gate.release(stripe);
                }));
            }

            gate.wait();
            assert_eq!(values[0].load(Ordering::Relaxed), 1);
            assert_eq!(values[1].load(Ordering::Relaxed), 2);
            gate.idle.reclaimed.store(true, Ordering::Release);
            for release in releases {
                release.join().unwrap();
            }
        });
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn loom_multiple_waiters_survive_reopen_before_every_waiter_resumes() {
        let mut model = loom::model::Builder::new();
        model.preemption_bound = Some(2);
        model.check(|| {
            let gate = Arc::new(ModelGate::<1>::new());
            gate.acquire(0);
            gate.seal();

            let first_gate = Arc::clone(&gate);
            let first = thread::spawn(move || {
                first_gate.wait();
                first_gate.reopen();
                first_gate.acquire(0);
                first_gate.release(0);
            });
            let second_gate = Arc::clone(&gate);
            let second = thread::spawn(move || second_gate.wait());

            gate.release(0);
            first.join().unwrap();
            second.join().unwrap();
        });
    }
}
