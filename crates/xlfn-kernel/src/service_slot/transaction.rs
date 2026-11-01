//! Consuming service transitions. Constructors stay in this module so a caller
//! cannot pair a foreign owner or drain gate with a reclamation capability.
use super::*;
use crate::sync::MutexGuard;
mod reclamation;
pub(super) use reclamation::{
    DrainedReplacement, PublishedServiceOwner, QuarantinedServiceOwner, QuiescedTxn, SealingTxn,
};

trait Rollback {
    fn rollback(self);
}

/// The Option only disarms RAII recovery when an operation completes. Protocol
/// states and their non-optional owners live in distinct consuming wrappers.
struct AbortGuard<S: Rollback> {
    pending: Option<S>,
}

impl<S: Rollback> AbortGuard<S> {
    fn new(state: S) -> Self {
        Self {
            pending: Some(state),
        }
    }

    fn state(&self) -> &S {
        self.pending
            .as_ref()
            .expect("a live transaction owns its recovery state")
    }

    fn complete(mut self) -> S {
        self.pending
            .take()
            .expect("a transaction completes only once")
    }
}

impl<S: Rollback> Drop for AbortGuard<S> {
    fn drop(&mut self) {
        if let Some(state) = self.pending.take() {
            state.rollback();
        }
    }
}

struct Initialization<'slot, C, R, E: Clone> {
    slot: &'slot GenerationServiceSlot<C, R, E>,
}

impl<C, R, E: Clone> Initialization<'_, C, R, E> {
    fn record_fault(&self, fault: ServiceFault<E>) {
        let fault = Arc::new(fault);
        let mut state = self.slot.state.lock();
        if !matches!(&*state, GenerationServiceState::Initializing) {
            crate::invariant::fail_stop();
        }
        *state = GenerationServiceState::InitFaulted { fault };
        self.slot.changed.notify_all();
    }
}

impl<C, R, E: Clone> Rollback for Initialization<'_, C, R, E> {
    fn rollback(self) {
        self.record_fault(ServiceFault::Panicked);
    }
}

#[must_use = "initialization must commit or retain a fault"]
pub(super) struct InitializingTxn<'slot, C, R, E: Clone> {
    recovery: AbortGuard<Initialization<'slot, C, R, E>>,
}

impl<'slot, C, R, E: Clone> InitializingTxn<'slot, C, R, E> {
    pub(super) fn commit(
        self,
        runtime: Box<R>,
        on_initialized: impl FnOnce(&R),
    ) -> GenerationServiceRead<'slot, R> {
        // [SS-1]: raw allocation ownership survives subsequent owner moves.
        let runtime = PublishedOwner::from_box(runtime);
        // Still unpublished: callback panic destroys only the local candidate
        // and the guard retains InitFaulted, outside the lifecycle lock [SS-5].
        on_initialized(runtime.as_ref());
        let pointer = NonNull::from(runtime.as_ref());
        let slot = self.recovery.state().slot;
        let mut state = slot.state.lock();
        if !matches!(&*state, GenerationServiceState::Initializing) {
            crate::invariant::fail_stop();
        }
        *state = GenerationServiceState::Ready {
            runtime: PublishedServiceOwner::publish(runtime),
        };
        slot.published.store(pointer.as_ptr(), Ordering::Release);
        drop(
            slot.readers
                .control()
                .wait_if_sealed()
                .unwrap_or_else(|_| crate::invariant::fail_stop())
                .reopen(),
        );
        // Publish and reserve the initializer's first permit under one lock.
        let permit = slot
            .readers
            .try_enter_current()
            .unwrap_or_else(|_| crate::invariant::fail_stop());
        self.recovery.complete();
        slot.changed.notify_all();
        drop(state);
        GenerationServiceRead {
            pointer,
            _permit: permit,
        }
    }

    pub(super) fn fail(self, error: E) -> ServiceSlotError<E> {
        // Error cloning can panic; the same guard then records Panicked.
        self.recovery
            .state()
            .record_fault(ServiceFault::Error(error.clone()));
        self.recovery.complete();
        ServiceSlotError::Fault(ServiceFault::Error(error))
    }
}

struct Transition<'slot, R> {
    // Unlock before dropping an unpublished candidate in a following field.
    _guard: MutexGuard<'slot, ()>,
    slot: &'slot ReplaceableServiceSlot<R>,
    previous: Option<usize>,
    target: usize,
}
impl<'slot, R> Transition<'slot, R> {
    fn new(slot: &'slot ReplaceableServiceSlot<R>) -> Self {
        let guard = slot.transition.lock();
        let previous = match slot.active.load(Ordering::Acquire) {
            0 => Some(0),
            1 => Some(1),
            _ => None,
        };
        Self {
            _guard: guard,
            slot,
            previous,
            target: previous.map_or(0, |index| 1 - index),
        }
    }
}

#[must_use]
pub(super) struct ReplacementTxn<'slot, R> {
    transition: Transition<'slot, R>,
    candidate: Box<R>,
}
#[must_use]
pub(super) struct PreparedReplacement<'slot, R> {
    transition: Transition<'slot, R>,
    candidate: Box<R>,
}
#[must_use]
pub(super) struct RetiringReplacement<'slot, R> {
    transition: Transition<'slot, R>,
}
#[must_use]
struct ClosingTxn<'slot, R> {
    transition: Transition<'slot, R>,
}

impl<'slot, R> ReplacementTxn<'slot, R> {
    fn new(slot: &'slot ReplaceableServiceSlot<R>, candidate: Box<R>) -> Self {
        Self {
            transition: Transition::new(slot),
            candidate,
        }
    }
    pub(super) fn prepare<E>(
        self,
        precommit: impl FnOnce(bool, Option<&R>) -> Result<(), E>,
    ) -> Result<PreparedReplacement<'slot, R>, E> {
        let lane = &self.transition.slot.lanes[self.transition.target];
        if !lane.readers.is_sealed()
            || lane.readers.active() != 0
            || !lane.published.load(Ordering::Acquire).is_null()
            || lane.owner.lock().is_some()
        {
            crate::invariant::fail_stop();
        }
        let old = self.transition.previous.map(|index| {
            let lane = &self.transition.slot.lanes[index];
            // The exact lane owner remains retained while this transition lock
            // excludes replacement and close; no arbitrary pointer is accepted.
            let pointer = NonNull::new(lane.published.load(Ordering::Acquire))
                .unwrap_or_else(|| crate::invariant::fail_stop());
            // SAFETY: this transition retains the exact slot and its lock.
            unsafe { pointer.as_ref() }
        });
        precommit(self.transition.previous.is_some(), old)?;
        Ok(PreparedReplacement {
            transition: self.transition,
            candidate: self.candidate,
        })
    }
}
impl<'slot, R> PreparedReplacement<'slot, R> {
    pub(super) fn publish(self) -> RetiringReplacement<'slot, R> {
        let Self {
            transition,
            candidate,
        } = self;
        let lane = &transition.slot.lanes[transition.target];
        let owner = PublishedServiceOwner::publish(PublishedOwner::from_box(candidate));
        let pointer = owner.as_ptr();
        *lane.owner.lock() = Some(owner);
        lane.published.store(pointer, Ordering::Release);
        drop(
            lane.readers
                .control()
                .wait_if_sealed()
                .unwrap_or_else(|_| crate::invariant::fail_stop())
                .reopen(),
        );
        // This uninterrupted sequence has no user callbacks or fallible steps.
        transition
            .slot
            .active
            .store(transition.target as u8, Ordering::Release);
        if let Some(index) = transition.previous {
            drop(transition.slot.lanes[index].readers.control().seal());
        }
        RetiringReplacement { transition }
    }
}
impl<'slot, R> ClosingTxn<'slot, R> {
    fn new(slot: &'slot ReplaceableServiceSlot<R>) -> Self {
        Self {
            transition: Transition::new(slot),
        }
    }
    fn withdraw(self) -> RetiringReplacement<'slot, R> {
        self.transition
            .slot
            .active
            .store(NO_ACTIVE_LANE, Ordering::Release);
        if let Some(index) = self.transition.previous {
            drop(self.transition.slot.lanes[index].readers.control().seal());
        }
        RetiringReplacement {
            transition: self.transition,
        }
    }
}
impl<C, R, E: Clone> GenerationServiceSlot<C, R, E> {
    #[cold]
    pub(super) fn read_slow(
        &self,
        initialize: impl FnOnce(C) -> Result<Box<R>, E>,
        on_initialized: impl FnOnce(&R),
    ) -> Result<GenerationServiceRead<'_, R>, ServiceSlotError<E>> {
        let mut state = self.state.lock();

        loop {
            match std::mem::replace(&mut *state, GenerationServiceState::Closed) {
                ready @ GenerationServiceState::Ready { .. } => {
                    *state = ready;
                    drop(state);
                    if let Some(read) = self.read_if_ready() {
                        return Ok(read);
                    }
                    state = self.state.lock();
                }
                GenerationServiceState::InitFaulted { fault } => {
                    let observed = Arc::clone(&fault);
                    *state = GenerationServiceState::InitFaulted { fault };
                    drop(state);
                    return Err(ServiceSlotError::Fault(observed.as_ref().clone()));
                }
                GenerationServiceState::TeardownFaulted { fault, runtime } => {
                    let observed = Arc::clone(&fault);
                    *state = GenerationServiceState::TeardownFaulted { fault, runtime };
                    drop(state);
                    return Err(ServiceSlotError::Fault(observed.as_ref().clone()));
                }
                waiting @ (GenerationServiceState::Initializing
                | GenerationServiceState::Sealing) => {
                    *state = waiting;
                    #[cfg(test)]
                    if let Some(observed) = self.before_read_wait.lock().take() {
                        observed.send(()).unwrap();
                    }
                    self.changed.wait(&mut state);
                }
                GenerationServiceState::Cold { config } => {
                    *state = GenerationServiceState::Initializing;
                    drop(state);

                    let transaction = InitializingTxn {
                        recovery: AbortGuard::new(Initialization { slot: self }),
                    };
                    let candidate = initialize(config);

                    match candidate {
                        Ok(runtime) => {
                            return Ok(transaction.commit(runtime, on_initialized));
                        }
                        Err(error) => return Err(transaction.fail(error)),
                    }
                }
                GenerationServiceState::Closed => return Err(ServiceSlotError::Closed),
            }
        }
    }

    /// Withdraws publication, drains every non-owning reader, and transfers
    /// the unique runtime owner to the returned seal token.
    pub fn seal<S>(
        &self,
        empty: impl FnOnce() -> S,
        shutdown: impl FnOnce(&R) -> Result<S, E>,
    ) -> Result<ServiceSeal<R, S>, ServiceSlotError<E>> {
        match self.begin_seal()? {
            Some(transaction) => transaction.wait().finish(shutdown),
            None => Ok(ServiceSeal::Empty(empty())),
        }
    }
}

impl<R> ReplaceableServiceSlot<R> {
    /// Replaces the currently active service with `candidate`.
    ///
    /// The `precommit` closure runs before candidate installation into the inactive lane,
    /// passing `(had_previous, old_service_ref)`. If `precommit` returns an error or panics,
    /// the inactive lane is completely untouched and the active lane continues uninterrupted.
    ///
    /// Once `precommit` succeeds, the candidate is installed, the inactive lane reopened,
    /// and the active lane atomically switches to the new lane.
    /// The previous lane is sealed, drained, unlinked, and returned as `RetiredService<R>`.
    pub fn replace_with<E>(
        &self,
        candidate: Box<R>,
        precommit: impl FnOnce(bool, Option<&R>) -> Result<(), E>,
    ) -> Result<Option<RetiredService<R>>, E> {
        Ok(ReplacementTxn::new(self, candidate)
            .prepare(precommit)?
            .publish()
            .wait()
            .reclaim())
    }

    /// Closes the active lane, drains all active readers, unlinks the service,
    /// and returns the retired `Box<R>` if any was active.
    pub fn close(&self) -> Option<RetiredService<R>> {
        ClosingTxn::new(self).withdraw().wait().reclaim()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc as StdArc, atomic::AtomicUsize};

    struct Counted {
        value: usize,
        drops: StdArc<AtomicUsize>,
    }

    impl Drop for Counted {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn miri_abandoned_seal_retains_its_owner_before_and_after_drain() {
        for drained in [false, true] {
            let drops = StdArc::new(AtomicUsize::new(0));
            let mut slot = GenerationServiceSlot::<(), Counted, ()>::new();
            slot.arm(()).unwrap();
            let read = slot
                .read(
                    |_| {
                        Ok(Box::new(Counted {
                            value: 17,
                            drops: StdArc::clone(&drops),
                        }))
                    },
                    |_| {},
                )
                .unwrap();
            let pointer = NonNull::from(&*read).as_ptr();
            let transaction = slot.begin_seal().unwrap().unwrap();
            assert!(slot.read_if_ready().is_none());
            if drained {
                drop(read);
                drop(transaction.wait());
            } else {
                drop(transaction);
                // Recovery must not move back into a Box or destroy a value
                // still accessed through an admitted raw read capability.
                assert_eq!(read.value, 17);
                drop(read);
            }
            assert_eq!(drops.load(Ordering::Relaxed), 0);
            assert!(slot.arm(()).is_err());
            let GenerationServiceState::TeardownFaulted { runtime, fault } =
                std::mem::replace(slot.state.get_mut(), GenerationServiceState::Closed)
            else {
                panic!("abandonment must quarantine the owner")
            };
            assert_eq!(*fault, ServiceFault::Panicked);
            let owner = runtime.recover_for_test();
            assert_eq!(owner.as_ptr(), pointer);
            // Test-only cleanup after the read is gone and slot is exclusive.
            drop(owner.into_box());
            assert_eq!(drops.load(Ordering::Relaxed), 1);
        }
    }

    #[test]
    fn miri_abandoned_lane_retirement_keeps_the_lane_owner_until_drain() {
        let drops = StdArc::new(AtomicUsize::new(0));
        let slot = ReplaceableServiceSlot::new();
        slot.replace_with(
            Box::new(Counted {
                value: 23,
                drops: StdArc::clone(&drops),
            }),
            |_, _| Ok::<_, ()>(()),
        )
        .unwrap();
        let read = slot.read_if_ready().unwrap();
        let pointer = NonNull::from(&*read).as_ptr();
        let retirement = ClosingTxn::new(&slot).withdraw();
        drop(retirement);
        assert_eq!(read.value, 23);
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        drop(read);
        assert_eq!(
            slot.lanes[0].owner.lock().as_ref().unwrap().as_ptr(),
            pointer
        );
        // Abandonment leaves the allocation in its sealed lane. Exclusive
        // slot destruction drains and releases it exactly once.
        drop(slot);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }
}

#[cfg(test)]
mod replacement_tests {
    use super::*;
    use std::sync::{Arc as StdArc, Weak, atomic::AtomicUsize};

    struct Probe {
        slot: Weak<ReplaceableServiceSlot<Probe>>,
        drops: StdArc<AtomicUsize>,
    }
    impl Drop for Probe {
        fn drop(&mut self) {
            if let Some(slot) = self.slot.upgrade() {
                assert!(
                    slot.transition.try_lock().is_some(),
                    "candidate destructor runs outside transition lock"
                );
            }
            self.drops.fetch_add(1, Ordering::Relaxed);
        }
    }
    #[test]
    fn miri_prepared_replacement_abandonment_and_precommit_error_release_candidate_unlocked() {
        let slot = StdArc::new(ReplaceableServiceSlot::new());
        let drops = StdArc::new(AtomicUsize::new(0));
        let candidate = || {
            Box::new(Probe {
                slot: StdArc::downgrade(&slot),
                drops: StdArc::clone(&drops),
            })
        };
        assert!(
            ReplacementTxn::new(&slot, candidate())
                .prepare(|_, _| Err(()))
                .is_err()
        );
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        let prepared = ReplacementTxn::new(&slot, candidate())
            .prepare(|_, _| Ok::<_, ()>(()))
            .unwrap();
        drop(prepared);
        assert_eq!(drops.load(Ordering::Relaxed), 2);
        assert!(slot.read_if_ready().is_none());
        assert!(slot.reset().is_ok());
    }
    #[test]
    fn miri_precommit_panic_releases_candidate_after_transition_lock() {
        let slot = StdArc::new(ReplaceableServiceSlot::new());
        let drops = StdArc::new(AtomicUsize::new(0));
        let candidate = Box::new(Probe {
            slot: StdArc::downgrade(&slot),
            drops: StdArc::clone(&drops),
        });
        let worker = StdArc::clone(&slot);
        assert!(
            std::thread::spawn(move || {
                let _prepared = ReplacementTxn::new(&worker, candidate)
                    .prepare(|_, _| -> Result<(), ()> { panic!("injected precommit panic") });
            })
            .join()
            .is_err()
        );
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert!(slot.reset().is_ok());
    }
    // Intentional retention is the specified outcome of forget; not a leak-free
    // Miri execution. No forgotten authority is converted back into ownership.
    #[test]
    #[cfg(not(miri))]
    #[allow(
        clippy::mem_forget,
        reason = "assert forgotten prepared authority retains its candidate and lock"
    )]
    fn forgotten_prepared_replacement_does_not_publish_or_destroy_its_candidate() {
        let slot = StdArc::new(ReplaceableServiceSlot::new());
        let drops = StdArc::new(AtomicUsize::new(0));
        let candidate = Box::new(Probe {
            slot: StdArc::downgrade(&slot),
            drops: StdArc::clone(&drops),
        });
        let prepared = ReplacementTxn::new(&slot, candidate)
            .prepare(|_, _| Ok::<_, ()>(()))
            .unwrap();
        std::mem::forget(prepared);
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        assert!(slot.read_if_ready().is_none());
        assert!(slot.transition.try_lock().is_none());
    }
    #[test]
    fn miri_slot_destruction_reclaims_published_owners_after_drain() {
        let drops = StdArc::new(AtomicUsize::new(0));
        let slot = ReplaceableServiceSlot::new();
        slot.replace_with(
            Box::new(Probe {
                slot: Weak::new(),
                drops: StdArc::clone(&drops),
            }),
            |_, _| Ok::<_, ()>(()),
        )
        .unwrap();
        drop(slot);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        let slot = GenerationServiceSlot::<(), Probe, ()>::new();
        slot.arm(()).unwrap();
        drop(
            slot.read(
                |_| {
                    Ok(Box::new(Probe {
                        slot: Weak::new(),
                        drops: StdArc::clone(&drops),
                    }))
                },
                |_| {},
            )
            .unwrap(),
        );
        drop(slot);
        assert_eq!(drops.load(Ordering::Relaxed), 2);
    }
}
