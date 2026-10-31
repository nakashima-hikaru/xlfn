//! Consuming service transitions. Constructors stay in this module so a caller
//! cannot pair a foreign owner or drain gate with a reclamation capability.
use super::*;
use crate::sync::MutexGuard;

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
        *state = GenerationServiceState::Ready { runtime };
        slot.published.store(pointer.as_ptr(), Ordering::Release);
        slot.readers
            .reopen()
            .unwrap_or_else(|_| crate::invariant::fail_stop());
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

struct Sealing<'slot, C, R, E: Clone> {
    slot: &'slot GenerationServiceSlot<C, R, E>,
    runtime: PublishedOwner<R>,
}

impl<C, R, E: Clone> Sealing<'_, C, R, E> {
    fn retain(self, fault: Arc<ServiceFault<E>>) {
        let mut state = self.slot.state.lock();
        if !matches!(&*state, GenerationServiceState::Sealing) {
            crate::invariant::fail_stop();
        }
        *state = GenerationServiceState::TeardownFaulted {
            fault,
            runtime: ManuallyDrop::new(self.runtime),
        };
        self.slot.changed.notify_all();
    }
}

impl<C, R, E: Clone> Rollback for Sealing<'_, C, R, E> {
    fn rollback(self) {
        // Also valid BEFORE reader drain: retain, never destroy, the owner.
        self.retain(Arc::new(ServiceFault::Panicked));
    }
}

#[must_use = "wait for this service's readers before finishing shutdown"]
pub(super) struct SealingTxn<'slot, C, R, E: Clone> {
    recovery: AbortGuard<Sealing<'slot, C, R, E>>,
}

#[must_use = "finish shutdown or retain the quiesced owner as a fault"]
pub(super) struct QuiescedTxn<'slot, C, R, E: Clone> {
    recovery: AbortGuard<Sealing<'slot, C, R, E>>,
}

impl<'slot, C, R, E: Clone> SealingTxn<'slot, C, R, E> {
    pub(super) fn wait(self) -> QuiescedTxn<'slot, C, R, E> {
        // [SS-4]: the owner and the gate originate from the SAME withdrawal.
        self.recovery.state().slot.readers.wait_until_idle();
        QuiescedTxn {
            recovery: self.recovery,
        }
    }
}

impl<C, R, E: Clone> QuiescedTxn<'_, C, R, E> {
    pub(super) fn finish<S>(
        self,
        shutdown: impl FnOnce(&R) -> Result<S, E>,
    ) -> Result<ServiceSeal<R, S>, ServiceSlotError<E>> {
        let result = shutdown(self.recovery.state().runtime.as_ref());
        match result {
            Ok(sealed) => {
                let slot = self.recovery.state().slot;
                let mut state = slot.state.lock();
                *state = GenerationServiceState::Closed;
                slot.changed.notify_all();
                drop(state);
                let Sealing { runtime, .. } = self.recovery.complete();
                // Only this consuming, quiesced state may recover the Box.
                Ok(ServiceSeal::Present {
                    runtime: runtime.into_box(),
                    sealed,
                })
            }
            Err(error) => {
                // Clone stays outside the lock AND under the recovery guard.
                let fault = Arc::new(ServiceFault::Error(error.clone()));
                self.recovery.complete().retain(fault);
                Err(ServiceSlotError::Fault(ServiceFault::Error(error)))
            }
        }
    }
}

struct LaneTransition<'slot, R> {
    slot: &'slot ReplaceableServiceSlot<R>,
    guard: MutexGuard<'slot, ()>,
}

impl<'slot, R> LaneTransition<'slot, R> {
    fn new(slot: &'slot ReplaceableServiceSlot<R>) -> Self {
        Self {
            slot,
            guard: slot.transition.lock(),
        }
    }

    fn withdraw_to(&self, next: u8) -> Option<LaneRetirement<'_, R>> {
        let previous = self.slot.active.load(Ordering::Acquire);
        if next > NO_ACTIVE_LANE || (previous < 2 && previous == next) {
            crate::invariant::fail_stop();
        }
        // The factory itself withdraws selection before issuing retirement;
        // callers cannot mint a drain for an arbitrarily selected lane.
        self.slot.active.store(next, Ordering::Release);
        if previous >= 2 {
            return None;
        }
        let lane = &self.slot.lanes[previous as usize];
        lane.readers.seal();
        Some(LaneRetirement {
            lane,
            _transition: &self.guard,
        })
    }
}

/// Active-index withdrawal has excluded new selection of this lane. Publication
/// stays intact until already-admitted readers have fetched their pointer.
#[must_use = "drain the retired lane before recovering its owner"]
pub(super) struct LaneRetirement<'txn, R> {
    lane: &'txn ReplaceableLane<R>,
    _transition: &'txn MutexGuard<'txn, ()>,
}

#[must_use = "recover the exact drained lane while its transition lock is held"]
pub(super) struct DrainedLane<'txn, R> {
    lane: &'txn ReplaceableLane<R>,
    _transition: &'txn MutexGuard<'txn, ()>,
}

impl<'txn, R> LaneRetirement<'txn, R> {
    pub(super) fn wait(self) -> DrainedLane<'txn, R> {
        self.lane.readers.wait_until_idle();
        DrainedLane {
            lane: self.lane,
            _transition: self._transition,
        }
    }
}

impl<R> DrainedLane<'_, R> {
    pub(super) fn reclaim(self) -> RetiredService<R> {
        self.lane
            .published
            .store(std::ptr::null_mut(), Ordering::Release);
        let owner = self
            .lane
            .owner
            .lock()
            .take()
            .unwrap_or_else(|| crate::invariant::fail_stop());
        RetiredService::new(owner.into_box())
    }
}

impl<C, R, E: Clone> GenerationServiceSlot<C, R, E> {
    #[cold]
    pub(super) fn read_slow(
        &self,
        initialize: impl FnOnce(C) -> Result<Box<R>, E>,
        on_initialized: impl FnOnce(&R),
    ) -> Result<GenerationServiceRead<'_, R>, ServiceSlotError<E>> {
        let mut initialize = Some(initialize);
        let mut on_initialized = Some(on_initialized);
        let mut state = self.state.lock();

        loop {
            match &*state {
                GenerationServiceState::Ready { .. } => {
                    drop(state);
                    if let Some(read) = self.read_if_ready() {
                        return Ok(read);
                    }
                    state = self.state.lock();
                }
                GenerationServiceState::InitFaulted { fault }
                | GenerationServiceState::TeardownFaulted { fault, .. } => {
                    let fault = Arc::clone(fault);
                    drop(state);
                    return Err(ServiceSlotError::Fault(fault.as_ref().clone()));
                }
                GenerationServiceState::Initializing | GenerationServiceState::Sealing => {
                    #[cfg(test)]
                    if let Some(observed) = self.before_read_wait.lock().take() {
                        observed.send(()).unwrap();
                    }
                    self.changed.wait(&mut state);
                }
                GenerationServiceState::Cold { .. } => {
                    let GenerationServiceState::Cold { config } =
                        std::mem::replace(&mut *state, GenerationServiceState::Closed)
                    else {
                        unreachable!("the service state remains Cold while holding its lock");
                    };
                    *state = GenerationServiceState::Initializing;
                    drop(state);

                    let transaction = InitializingTxn {
                        recovery: AbortGuard::new(Initialization { slot: self }),
                    };
                    let candidate = initialize
                        .take()
                        .expect("a service initializer is consumed exactly once")(
                        config
                    );

                    match candidate {
                        Ok(runtime) => {
                            let on_initialized = on_initialized
                                .take()
                                .expect("a service initializer callback is consumed once");
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

    fn begin_seal(&self) -> Result<Option<SealingTxn<'_, C, R, E>>, ServiceSlotError<E>> {
        let mut state = self.state.lock();
        while matches!(
            *state,
            GenerationServiceState::Initializing | GenerationServiceState::Sealing
        ) {
            self.changed.wait(&mut state);
        }

        if let GenerationServiceState::TeardownFaulted { fault, .. } = &*state {
            let fault = Arc::clone(fault);
            drop(state);
            return Err(ServiceSlotError::Fault(fault.as_ref().clone()));
        }

        Ok(
            match std::mem::replace(&mut *state, GenerationServiceState::Sealing) {
                GenerationServiceState::Ready { runtime } => {
                    // Formal theorem [SS-3]: Withdraw publication and seal readers gate
                    // before waiting for drain, preventing any late reader admission.
                    self.readers.seal();
                    self.published
                        .store(std::ptr::null_mut(), Ordering::Release);
                    Some(SealingTxn {
                        recovery: AbortGuard::new(Sealing {
                            slot: self,
                            runtime,
                        }),
                    })
                }
                retired @ (GenerationServiceState::Cold { .. }
                | GenerationServiceState::InitFaulted { .. }
                | GenerationServiceState::Closed) => {
                    *state = GenerationServiceState::Closed;
                    self.changed.notify_all();
                    drop(state);
                    drop(retired);
                    return Ok(None);
                }
                GenerationServiceState::Initializing
                | GenerationServiceState::Sealing
                | GenerationServiceState::TeardownFaulted { .. } => {
                    unreachable!()
                }
            },
        )
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
        let transition = LaneTransition::new(self);

        let current_index = self.active.load(Ordering::Acquire);
        let (target_index, had_previous) = match current_index {
            0 => (1, true),
            1 => (0, true),
            _ => (0, false),
        };

        let target_lane = &self.lanes[target_index];
        if !target_lane.readers.is_sealed()
            || target_lane.readers.active() != 0
            || !target_lane.published.load(Ordering::Acquire).is_null()
            || target_lane.owner.lock().is_some()
        {
            crate::invariant::fail_stop();
        }

        let current_ref = if had_previous {
            let current_lane = &self.lanes[current_index as usize];
            let ptr = current_lane.published.load(Ordering::Acquire);
            if ptr.is_null() {
                crate::invariant::fail_stop();
            }
            // SAFETY: `ptr` is non-null and points to the live service Box owned by
            // `current_lane.owner` whose publication gate is open. The transition lock
            // is held, preventing concurrent unlinking, sealing, or modification.
            Some(unsafe { &*ptr })
        } else {
            None
        };

        precommit(had_previous, current_ref)?;

        let candidate = PublishedOwner::from_box(candidate);
        let pointer = {
            let mut owner = target_lane.owner.lock();
            let slot = owner.insert(candidate);
            NonNull::from(slot.as_ref())
        };
        target_lane
            .published
            .store(pointer.as_ptr(), Ordering::Release);
        target_lane
            .readers
            .reopen()
            .unwrap_or_else(|_| crate::invariant::fail_stop());

        // Linearization and old-lane withdrawal precede its drain token.
        Ok(transition
            .withdraw_to(target_index as u8)
            .map(|retired| retired.wait().reclaim()))
    }

    /// Closes the active lane, drains all active readers, unlinks the service,
    /// and returns the retired `Box<R>` if any was active.
    pub fn close(&self) -> Option<RetiredService<R>> {
        let transition = LaneTransition::new(self);
        transition
            .withdraw_to(NO_ACTIVE_LANE)
            .map(|retired| retired.wait().reclaim())
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
            let owner = ManuallyDrop::into_inner(runtime);
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
        let transition = LaneTransition::new(&slot);
        let retirement = transition.withdraw_to(NO_ACTIVE_LANE).unwrap();
        drop(retirement);
        assert_eq!(read.value, 23);
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        drop(read);
        assert_eq!(
            slot.lanes[0].owner.lock().as_ref().unwrap().as_ptr(),
            pointer
        );
        drop(transition);
        // Abandonment leaves the allocation in its sealed lane. Exclusive
        // slot destruction drains and releases it exactly once.
        drop(slot);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }
}
