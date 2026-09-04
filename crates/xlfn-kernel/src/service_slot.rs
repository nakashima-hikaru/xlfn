//! Generic generation-scoped lazy service publication.
//!
//! The slot is the sole owner of the published service. Readers hold a
//! non-owning pointer together with a drain permit; sealing first withdraws
//! publication, then waits for every permit before transferring the `Box` to
//! the teardown owner. No read capability participates in shared ownership.

use crate::drain_gate::{DEFAULT_STRIPE_COUNT, StripedDrainGate, StripedDrainPermit};
use parking_lot::{Condvar, Mutex};
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::ops::Deref;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicPtr, AtomicU8, Ordering};

/// Number of reader stripes for the generation service slot.
pub const SERVICE_READER_STRIPES: usize = DEFAULT_STRIPE_COUNT;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServiceFault<E> {
    Error(E),
    Panicked,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServiceSlotError<E> {
    Closed,
    Fault(ServiceFault<E>),
}

/// Shared lifecycle vocabulary for one generation-scoped lazy service.
pub enum GenerationServiceState<C, R, E> {
    Closed,
    Cold {
        config: C,
    },
    Initializing,
    Ready {
        runtime: Box<R>,
    },
    Sealing,
    InitFaulted {
        fault: ServiceFault<E>,
    },
    TeardownFaulted {
        fault: ServiceFault<E>,
        runtime: ManuallyDrop<Box<R>>,
    },
}

/// A non-owning read capability over one published generation service.
pub struct GenerationServiceRead<'slot, R> {
    pointer: NonNull<R>,
    _permit: StripedDrainPermit<'slot, SERVICE_READER_STRIPES>,
}

impl<R> Deref for GenerationServiceRead<'_, R> {
    type Target = R;

    #[inline]
    fn deref(&self) -> &Self::Target {
        // SAFETY: the pointer originates from the slot-owned Box. The permit
        // prevents seal/reclamation until this guard is dropped.
        unsafe { self.pointer.as_ref() }
    }
}

/// Ownership transferred out of a successfully sealed service slot.
pub enum ServiceSeal<R, S> {
    Empty(S),
    Present { runtime: Box<R>, sealed: S },
}

impl<R, S> ServiceSeal<R, S> {
    pub fn into_parts(self) -> (Option<Box<R>>, S) {
        match self {
            Self::Empty(sealed) => (None, sealed),
            Self::Present { runtime, sealed } => (Some(runtime), sealed),
        }
    }
}

struct InitializingTxn<'slot, C, R, E: Clone> {
    slot: &'slot GenerationServiceSlot<C, R, E>,
    committed: bool,
}

impl<'slot, C, R, E: Clone> InitializingTxn<'slot, C, R, E> {
    fn commit(
        mut self,
        runtime: Box<R>,
        on_initialized: impl FnOnce(&R),
    ) -> GenerationServiceRead<'slot, R> {
        // Keep the callback outside the state lock. If it panics, Drop owns
        // the transition to InitFaulted and the local Box is reclaimed.
        on_initialized(runtime.as_ref());
        let pointer = NonNull::from(runtime.as_ref());

        let mut state = self.slot.state.lock();
        match &*state {
            GenerationServiceState::Initializing => {}
            _ => unreachable!("initialization transaction lost its state owner"),
        }
        *state = GenerationServiceState::Ready { runtime };
        self.slot
            .published
            .store(pointer.as_ptr(), Ordering::Release);
        self.slot
            .readers
            .reopen()
            .unwrap_or_else(|_| crate::invariant::fail_stop());
        self.committed = true;
        self.slot.changed.notify_all();
        drop(state);

        self.slot
            .read_if_ready()
            .expect("a committed service is immediately readable")
    }

    fn fail(mut self, error: E) -> ServiceSlotError<E> {
        self.record_fault(ServiceFault::Error(error.clone()));
        self.committed = true;
        ServiceSlotError::Fault(ServiceFault::Error(error))
    }

    fn record_fault(&mut self, fault: ServiceFault<E>) {
        let mut state = self.slot.state.lock();
        if matches!(&*state, GenerationServiceState::Initializing) {
            *state = GenerationServiceState::InitFaulted { fault };
            self.slot.changed.notify_all();
        }
    }
}

impl<C, R, E: Clone> Drop for InitializingTxn<'_, C, R, E> {
    fn drop(&mut self) {
        if !self.committed {
            self.record_fault(ServiceFault::Panicked);
        }
    }
}

struct SealingTxn<'slot, C, R, E: Clone> {
    slot: &'slot GenerationServiceSlot<C, R, E>,
    runtime: Option<Box<R>>,
    committed: bool,
}

impl<C, R, E: Clone> SealingTxn<'_, C, R, E> {
    fn finish<S>(
        mut self,
        shutdown: impl FnOnce(&R) -> Result<S, E>,
    ) -> Result<ServiceSeal<R, S>, ServiceSlotError<E>> {
        let runtime = self
            .runtime
            .as_ref()
            .expect("a sealing transaction owns its runtime root");
        let result = shutdown(runtime.as_ref());
        let mut state = self.slot.state.lock();

        match result {
            Ok(sealed) => {
                *state = GenerationServiceState::Closed;
                self.committed = true;
                self.slot.changed.notify_all();
                drop(state);
                let runtime = self
                    .runtime
                    .take()
                    .expect("a successful seal transfers its runtime root");
                Ok(ServiceSeal::Present { runtime, sealed })
            }
            Err(error) => {
                let runtime = self
                    .runtime
                    .take()
                    .expect("a failed seal retains its runtime root");
                *state = GenerationServiceState::TeardownFaulted {
                    fault: ServiceFault::Error(error.clone()),
                    runtime: ManuallyDrop::new(runtime),
                };
                self.committed = true;
                self.slot.changed.notify_all();
                Err(ServiceSlotError::Fault(ServiceFault::Error(error)))
            }
        }
    }

    fn record_fault(&mut self) {
        let Some(runtime) = self.runtime.take() else {
            return;
        };
        let mut state = self.slot.state.lock();
        if matches!(&*state, GenerationServiceState::Sealing) {
            *state = GenerationServiceState::TeardownFaulted {
                fault: ServiceFault::Panicked,
                runtime: ManuallyDrop::new(runtime),
            };
            self.slot.changed.notify_all();
        }
    }
}

impl<C, R, E: Clone> Drop for SealingTxn<'_, C, R, E> {
    fn drop(&mut self) {
        if !self.committed {
            self.record_fault();
        }
    }
}

/// Common state-machine kernel for a generation-scoped lazy service.
pub struct GenerationServiceSlot<C, R, E> {
    published: AtomicPtr<R>,
    readers: StripedDrainGate<SERVICE_READER_STRIPES>,
    state: Mutex<GenerationServiceState<C, R, E>>,
    changed: Condvar,
    _runtime: PhantomData<R>,
}

impl<C, R, E> GenerationServiceSlot<C, R, E> {
    pub const fn new() -> Self {
        Self {
            published: AtomicPtr::new(std::ptr::null_mut()),
            readers: StripedDrainGate::new_sealed(),
            state: Mutex::new(GenerationServiceState::Closed),
            changed: Condvar::new(),
            _runtime: PhantomData,
        }
    }

    pub fn arm(&self, config: C) -> Result<(), ServiceSlotError<E>> {
        let mut state = self.state.lock();
        if !matches!(*state, GenerationServiceState::Closed)
            || !self.readers.is_sealed()
            || self.readers.active() != 0
            || !self.published.load(Ordering::Acquire).is_null()
        {
            return Err(ServiceSlotError::Closed);
        }
        *state = GenerationServiceState::Cold { config };
        self.changed.notify_all();
        Ok(())
    }

    pub fn disarm(&self) -> Result<(), ServiceSlotError<E>> {
        let mut state = self.state.lock();
        match &*state {
            GenerationServiceState::Cold { .. } | GenerationServiceState::InitFaulted { .. } => {
                *state = GenerationServiceState::Closed;
                self.changed.notify_all();
                Ok(())
            }
            GenerationServiceState::Closed => Ok(()),
            _ => Err(ServiceSlotError::Closed),
        }
    }

    pub fn is_none(&self) -> bool {
        self.published.load(Ordering::Acquire).is_null()
            && matches!(
                *self.state.lock(),
                GenerationServiceState::Closed | GenerationServiceState::InitFaulted { .. }
            )
    }

    pub fn read_if_ready(&self) -> Option<GenerationServiceRead<'_, R>> {
        let permit = self.readers.try_enter_current().ok()?;
        let pointer = NonNull::new(self.published.load(Ordering::Acquire));
        match pointer {
            Some(pointer) => Some(GenerationServiceRead {
                pointer,
                _permit: permit,
            }),
            None => {
                drop(permit);
                None
            }
        }
    }

    pub fn with_published(&self, callback: impl FnOnce(Option<&R>)) {
        let published = self.read_if_ready();
        callback(published.as_deref());
    }
}

impl<C, R, E> Default for GenerationServiceSlot<C, R, E> {
    fn default() -> Self {
        Self::new()
    }
}

impl<C, R, E: Clone> GenerationServiceSlot<C, R, E> {
    pub fn read(
        &self,
        initialize: impl FnOnce(C) -> Result<Box<R>, E>,
        on_initialized: impl FnOnce(&R),
    ) -> Result<GenerationServiceRead<'_, R>, ServiceSlotError<E>> {
        if let Some(read) = self.read_if_ready() {
            return Ok(read);
        }
        self.read_slow(initialize, on_initialized)
    }

    #[cold]
    fn read_slow(
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
                    return Err(ServiceSlotError::Fault(fault.clone()));
                }
                GenerationServiceState::Initializing | GenerationServiceState::Sealing => {
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
                        slot: self,
                        committed: false,
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
        let runtime = {
            let mut state = self.state.lock();
            while matches!(
                *state,
                GenerationServiceState::Initializing | GenerationServiceState::Sealing
            ) {
                self.changed.wait(&mut state);
            }

            match std::mem::replace(&mut *state, GenerationServiceState::Sealing) {
                GenerationServiceState::Ready { runtime } => {
                    self.readers.seal();
                    self.published
                        .store(std::ptr::null_mut(), Ordering::Release);
                    runtime
                }
                GenerationServiceState::Cold { .. }
                | GenerationServiceState::InitFaulted { .. } => {
                    *state = GenerationServiceState::Closed;
                    self.changed.notify_all();
                    return Ok(ServiceSeal::Empty(empty()));
                }
                GenerationServiceState::Closed => {
                    *state = GenerationServiceState::Closed;
                    return Ok(ServiceSeal::Empty(empty()));
                }
                GenerationServiceState::TeardownFaulted { fault, runtime } => {
                    *state = GenerationServiceState::TeardownFaulted {
                        fault: fault.clone(),
                        runtime,
                    };
                    return Err(ServiceSlotError::Fault(fault));
                }
                GenerationServiceState::Initializing | GenerationServiceState::Sealing => {
                    unreachable!()
                }
            }
        };

        self.readers.wait_until_idle();
        SealingTxn {
            slot: self,
            runtime: Some(runtime),
            committed: false,
        }
        .finish(shutdown)
    }
}

/// Sentinel value for `ReplaceableServiceSlot::active` indicating no lane is active.
pub const NO_ACTIVE_LANE: u8 = 2;

/// A non-owning read capability over a replaceable service.
pub struct ReplaceableServiceRead<'slot, R> {
    pointer: NonNull<R>,
    _permit: StripedDrainPermit<'slot, SERVICE_READER_STRIPES>,
}

impl<R> Deref for ReplaceableServiceRead<'_, R> {
    type Target = R;

    #[inline]
    fn deref(&self) -> &Self::Target {
        // SAFETY: the pointer originates from the lane-owned Box. The permit
        // prevents seal/reclamation of this lane until this guard is dropped.
        unsafe { self.pointer.as_ref() }
    }
}

/// A retired service unlinked, drained, and transferred out of a replaceable slot.
#[derive(Debug)]
pub struct RetiredService<R> {
    runtime: Box<R>,
}

impl<R> RetiredService<R> {
    pub(crate) fn new(runtime: Box<R>) -> Self {
        Self { runtime }
    }

    pub fn into_inner(self) -> Box<R> {
        self.runtime
    }
}

impl<R> Deref for RetiredService<R> {
    type Target = R;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.runtime
    }
}

/// One lane in a 2-lane replaceable service slot.
pub struct ReplaceableLane<R> {
    published: AtomicPtr<R>,
    readers: StripedDrainGate<SERVICE_READER_STRIPES>,
    owner: Mutex<Option<Box<R>>>,
    _runtime: PhantomData<R>,
}

impl<R> ReplaceableLane<R> {
    pub const fn new() -> Self {
        Self {
            published: AtomicPtr::new(std::ptr::null_mut()),
            readers: StripedDrainGate::new_sealed(),
            owner: Mutex::new(None),
            _runtime: PhantomData,
        }
    }
}

impl<R> Default for ReplaceableLane<R> {
    fn default() -> Self {
        Self::new()
    }
}

/// Two-lane ping-pong replaceable service slot.
///
/// Permits seamless replacement of an active service without shared ownership or
/// intermediate blackout:
/// 1. Precommit validation runs before candidate installation.
/// 2. Candidate is installed into the idle lane and its reader gate is reopened.
/// 3. Infallible linearization: active lane index atomically switches to the new lane.
/// 4. Old lane is sealed, drained, unlinked, and its Box transferred as `RetiredService<R>`.
pub struct ReplaceableServiceSlot<R> {
    lanes: [ReplaceableLane<R>; 2],
    active: AtomicU8,
    transition: Mutex<()>,
    _runtime: PhantomData<R>,
}

/// Error indicating that a replaceable service slot failed its reset invariant check.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResetError;

impl<R> ReplaceableServiceSlot<R> {
    pub const fn new() -> Self {
        Self {
            lanes: [ReplaceableLane::new(), ReplaceableLane::new()],
            active: AtomicU8::new(NO_ACTIVE_LANE),
            transition: Mutex::new(()),
            _runtime: PhantomData,
        }
    }

    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire) < 2
    }

    pub fn read_if_ready(&self) -> Option<ReplaceableServiceRead<'_, R>> {
        loop {
            let index = self.active.load(Ordering::Acquire);
            if index >= 2 {
                return None;
            }

            let lane = &self.lanes[index as usize];

            let permit = match lane.readers.try_enter_current() {
                Ok(permit) => permit,
                Err(_) => continue,
            };

            // Publication changed while entering? Revalidate active lane.
            if self.active.load(Ordering::Acquire) != index {
                drop(permit);
                continue;
            }

            let pointer = NonNull::new(lane.published.load(Ordering::Acquire))
                .unwrap_or_else(|| crate::invariant::fail_stop());

            return Some(ReplaceableServiceRead {
                pointer,
                _permit: permit,
            });
        }
    }

    pub fn with_published(&self, callback: impl FnOnce(Option<&R>)) {
        let published = self.read_if_ready();
        callback(published.as_deref());
    }

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
        let _guard = self.transition.lock();

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

        let pointer = NonNull::from(candidate.as_ref());
        *target_lane.owner.lock() = Some(candidate);
        target_lane
            .published
            .store(pointer.as_ptr(), Ordering::Release);
        target_lane
            .readers
            .reopen()
            .unwrap_or_else(|_| crate::invariant::fail_stop());

        // Linearization point: atomic publication switch
        self.active.store(target_index as u8, Ordering::Release);

        if had_previous {
            let old_lane = &self.lanes[current_index as usize];
            old_lane.readers.seal();
            old_lane.readers.wait_until_idle();
            old_lane
                .published
                .store(std::ptr::null_mut(), Ordering::Release);
            let old = old_lane
                .owner
                .lock()
                .take()
                .unwrap_or_else(|| crate::invariant::fail_stop());
            Ok(Some(RetiredService::new(old)))
        } else {
            Ok(None)
        }
    }

    /// Closes the active lane, drains all active readers, unlinks the service,
    /// and returns the retired Box<R> if any was active.
    pub fn close(&self) -> Option<RetiredService<R>> {
        let _guard = self.transition.lock();
        let current_index = self.active.load(Ordering::Acquire);
        if current_index >= 2 {
            return None;
        }
        self.active.store(NO_ACTIVE_LANE, Ordering::Release);

        let old_lane = &self.lanes[current_index as usize];
        old_lane.readers.seal();
        old_lane.readers.wait_until_idle();
        old_lane
            .published
            .store(std::ptr::null_mut(), Ordering::Release);
        let old = old_lane
            .owner
            .lock()
            .take()
            .unwrap_or_else(|| crate::invariant::fail_stop());
        Some(RetiredService::new(old))
    }

    /// Validates the reset invariant:
    /// `active == NO_ACTIVE_LANE`, both lanes sealed, both lanes idle, both owners empty.
    pub fn reset(&self) -> Result<(), ResetError> {
        let _guard = self.transition.lock();
        if self.active.load(Ordering::Acquire) != NO_ACTIVE_LANE {
            return Err(ResetError);
        }
        for lane in &self.lanes {
            if !lane.readers.is_sealed()
                || lane.readers.active() != 0
                || !lane.published.load(Ordering::Acquire).is_null()
                || lane.owner.lock().is_some()
            {
                return Err(ResetError);
            }
        }
        Ok(())
    }
}

impl<R> Default for ReplaceableServiceSlot<R> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R> Drop for ReplaceableServiceSlot<R> {
    fn drop(&mut self) {
        self.active.store(NO_ACTIVE_LANE, Ordering::Release);
        for lane in &self.lanes {
            lane.readers.seal();
            lane.readers.wait_until_idle();
            lane.published
                .store(std::ptr::null_mut(), Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        GenerationServiceSlot, GenerationServiceState, ReplaceableServiceSlot, ServiceFault,
        ServiceSeal, ServiceSlotError,
    };
    use static_assertions::{assert_impl_all, assert_not_impl_any};
    use std::cell::Cell;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    assert_impl_all!(ReplaceableServiceSlot<u32>: Send, Sync);
    assert_impl_all!(ReplaceableServiceSlot<Cell<u32>>: Send);
    assert_not_impl_any!(ReplaceableServiceSlot<Cell<u32>>: Sync);

    assert_impl_all!(GenerationServiceSlot<(), u32, ()>: Send, Sync);
    assert_impl_all!(GenerationServiceSlot<(), Cell<u32>, ()>: Send);
    assert_not_impl_any!(GenerationServiceSlot<(), Cell<u32>, ()>: Sync);

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct TestError(&'static str);

    struct NonCopyConfig(String);
    struct Service;

    type Slot = GenerationServiceSlot<NonCopyConfig, Service, TestError>;

    #[test]
    fn initialization_moves_a_non_copy_config_once() {
        let slot = Slot::new();
        slot.arm(NonCopyConfig("moved once".to_owned()))
            .expect("service slot can be armed");
        let read = slot
            .read(
                |config| {
                    assert_eq!(config.0, "moved once");
                    Ok(Box::new(Service))
                },
                |_| {},
            )
            .expect("service slot initializes from its moved config");
        std::hint::black_box(&*read);
    }

    #[test]
    fn initialization_panic_is_retained_and_wakes_waiters() {
        let slot = GenerationServiceSlot::<(), Service, TestError>::new();
        slot.arm(()).expect("service slot can be armed");
        let result = catch_unwind(AssertUnwindSafe(|| {
            let _ = slot.read(
                |_| -> Result<Box<Service>, TestError> { panic!("injected initializer panic") },
                |_| {},
            );
        }));
        assert!(result.is_err());
        assert!(matches!(
            &*slot.state.lock(),
            GenerationServiceState::InitFaulted {
                fault: ServiceFault::Panicked,
            }
        ));
        assert!(matches!(
            slot.read(|_| Ok(Box::new(Service)), |_| {}),
            Err(ServiceSlotError::Fault(ServiceFault::Panicked))
        ));
    }

    #[test]
    fn seal_transfers_the_unique_runtime_owner() {
        let slot = GenerationServiceSlot::<(), Service, TestError>::new();
        slot.arm(()).expect("service slot can be armed");
        let read = slot
            .read(|_| Ok(Box::new(Service)), |_| {})
            .expect("service slot initializes");
        drop(read);
        let sealed = slot.seal(|| (), |_| Ok(())).expect("service slot seals");
        assert!(matches!(sealed, ServiceSeal::Present { .. }));
        assert!(slot.is_none());
    }

    #[test]
    fn shutdown_panic_retains_runtime_as_teardown_fault() {
        let slot = GenerationServiceSlot::<(), Service, TestError>::new();
        slot.arm(()).expect("service slot can be armed");
        let read = slot
            .read(|_| Ok(Box::new(Service)), |_| {})
            .expect("service slot initializes");
        drop(read);
        let result = catch_unwind(AssertUnwindSafe(|| {
            let _ = slot.seal(
                || (),
                |_| -> Result<(), TestError> { panic!("injected shutdown panic") },
            );
        }));
        assert!(result.is_err());
        assert!(matches!(
            &*slot.state.lock(),
            GenerationServiceState::TeardownFaulted {
                fault: ServiceFault::Panicked,
                ..
            }
        ));
        assert!(matches!(
            slot.seal(|| (), |_| Ok(())),
            Err(ServiceSlotError::Fault(ServiceFault::Panicked))
        ));
    }

    #[test]
    fn replaceable_slot_initial_state() {
        let slot = ReplaceableServiceSlot::<String>::new();
        assert!(!slot.is_active());
        assert!(slot.read_if_ready().is_none());
        assert!(slot.reset().is_ok());
    }

    #[test]
    fn replaceable_slot_replace_and_ping_pong() {
        let slot = ReplaceableServiceSlot::<u32>::new();

        // 1st publish: empty -> lane 0
        let prev = slot
            .replace_with(Box::new(10), |had_prev, old| {
                assert!(!had_prev);
                assert!(old.is_none());
                Ok::<(), ()>(())
            })
            .expect("first publish succeeds");
        assert!(prev.is_none());
        assert!(slot.is_active());
        assert_eq!(*slot.read_if_ready().unwrap(), 10);

        // 2nd publish: lane 0 -> lane 1
        let prev = slot
            .replace_with(Box::new(20), |had_prev, old| {
                assert!(had_prev);
                assert_eq!(*old.unwrap(), 10);
                Ok::<(), ()>(())
            })
            .expect("second publish succeeds")
            .expect("previous service exists");
        assert_eq!(*prev, 10);
        assert_eq!(*slot.read_if_ready().unwrap(), 20);

        // 3rd publish: lane 1 -> lane 0 (ping-pong)
        let prev = slot
            .replace_with(Box::new(30), |had_prev, old| {
                assert!(had_prev);
                assert_eq!(*old.unwrap(), 20);
                Ok::<(), ()>(())
            })
            .expect("third publish succeeds")
            .expect("previous service exists");
        assert_eq!(*prev, 20);
        assert_eq!(*slot.read_if_ready().unwrap(), 30);
    }

    #[test]
    fn replaceable_slot_precommit_rollback() {
        let slot = ReplaceableServiceSlot::<u32>::new();
        slot.replace_with(Box::new(100), |_, _| Ok::<(), &'static str>(()))
            .unwrap();

        let err = slot
            .replace_with(Box::new(200), |_, _| Err::<(), &'static str>("rejected"))
            .unwrap_err();
        assert_eq!(err, "rejected");

        // Active service remains untouched
        assert_eq!(*slot.read_if_ready().unwrap(), 100);

        // Candidate lane can be reused cleanly afterwards
        let prev = slot
            .replace_with(Box::new(300), |had_prev, old| {
                assert!(had_prev);
                assert_eq!(*old.unwrap(), 100);
                Ok::<(), &'static str>(())
            })
            .unwrap()
            .unwrap();
        assert_eq!(*prev, 100);
        assert_eq!(*slot.read_if_ready().unwrap(), 300);
    }

    #[test]
    fn replaceable_slot_precommit_panic_leaves_active_and_inactive_intact() {
        let slot = ReplaceableServiceSlot::<u32>::new();
        slot.replace_with(Box::new(100), |_, _| Ok::<(), ()>(()))
            .unwrap();

        let result = catch_unwind(AssertUnwindSafe(|| {
            let _ = slot.replace_with(Box::new(200), |_, _| -> Result<(), ()> {
                panic!("precommit exploded");
            });
        }));
        assert!(result.is_err());

        // Active service remains untouched
        assert_eq!(*slot.read_if_ready().unwrap(), 100);

        // Inactive lane can still be used cleanly afterwards
        let prev = slot
            .replace_with(Box::new(300), |had_prev, old| {
                assert!(had_prev);
                assert_eq!(*old.unwrap(), 100);
                Ok::<(), ()>(())
            })
            .unwrap()
            .unwrap();
        assert_eq!(*prev, 100);
        assert_eq!(*slot.read_if_ready().unwrap(), 300);
    }

    #[test]
    fn replaceable_slot_concurrent_readers_and_drain() {
        use std::sync::Arc;
        let slot = Arc::new(ReplaceableServiceSlot::<u32>::new());
        slot.replace_with(Box::new(1), |_, _| Ok::<(), ()>(()))
            .unwrap();

        let (reader_entered_tx, reader_entered_rx) = std::sync::mpsc::channel();
        let (release_reader_tx, release_reader_rx) = std::sync::mpsc::channel();

        let reader_slot = Arc::clone(&slot);
        let reader_handle = std::thread::spawn(move || {
            let read = reader_slot.read_if_ready().unwrap();
            assert_eq!(*read, 1);
            reader_entered_tx.send(()).unwrap();
            release_reader_rx.recv().unwrap();
            drop(read);
        });

        reader_entered_rx.recv().unwrap();

        let replacer_slot = Arc::clone(&slot);
        let (replacer_finished_tx, replacer_finished_rx) = std::sync::mpsc::channel();
        let replacer_handle = std::thread::spawn(move || {
            let retired = replacer_slot
                .replace_with(Box::new(2), |_, _| Ok::<(), ()>(()))
                .unwrap()
                .unwrap();
            assert_eq!(*retired, 1);
            replacer_finished_tx.send(()).unwrap();
        });

        // Replacer should be waiting for reader to release its permit
        assert!(
            replacer_finished_rx
                .recv_timeout(std::time::Duration::from_millis(50))
                .is_err()
        );

        // Release reader
        release_reader_tx.send(()).unwrap();
        reader_handle.join().unwrap();

        // Replacer now unblocks and finishes
        replacer_finished_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        replacer_handle.join().unwrap();

        assert_eq!(*slot.read_if_ready().unwrap(), 2);
    }

    #[test]
    fn replaceable_slot_close_and_reset() {
        let slot = ReplaceableServiceSlot::<u32>::new();
        slot.replace_with(Box::new(42), |_, _| Ok::<(), ()>(()))
            .unwrap();

        let retired = slot.close().expect("close returns retired service");
        assert_eq!(*retired, 42);
        assert!(!slot.is_active());
        assert!(slot.read_if_ready().is_none());

        assert!(slot.reset().is_ok());

        // Can be re-opened with new service after reset
        slot.replace_with(Box::new(84), |_, _| Ok::<(), ()>(()))
            .unwrap();
        assert_eq!(*slot.read_if_ready().unwrap(), 84);
    }
}
