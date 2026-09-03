//! Generic generation-scoped lazy service publication.
//!
//! The slot is the sole owner of the published service. Readers hold a
//! non-owning pointer together with a drain permit; sealing first withdraws
//! publication, then waits for every permit before transferring the `Box` to
//! the teardown owner. No read capability participates in shared ownership.

use crate::drain_gate::{DEFAULT_STRIPE_COUNT, StripedDrainGate, StripedDrainPermit};
use parking_lot::{Condvar, Mutex};
use std::mem::ManuallyDrop;
use std::ops::Deref;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicPtr, Ordering};

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
}

impl<C, R, E> GenerationServiceSlot<C, R, E> {
    pub const fn new() -> Self {
        Self {
            published: AtomicPtr::new(std::ptr::null_mut()),
            readers: StripedDrainGate::new_sealed(),
            state: Mutex::new(GenerationServiceState::Closed),
            changed: Condvar::new(),
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

#[cfg(test)]
mod tests {
    use super::{
        GenerationServiceSlot, GenerationServiceState, ServiceFault, ServiceSeal, ServiceSlotError,
    };
    use std::panic::{AssertUnwindSafe, catch_unwind};

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
}
