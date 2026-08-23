//! Generic generation-scoped lazy service publication.
//!
//! This module owns the service-slot protocol only. The error type and the
//! service payload are supplied by the adapter crate, so the kernel does not
//! know about Excel, XLL diagnostics, or any concrete runtime service.

use arc_swap::ArcSwapOption;
use parking_lot::{Condvar, Mutex};
use std::mem::ManuallyDrop;
use std::ops::Deref;
use std::sync::Arc;

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
    Ready,
    Sealing,
    InitFaulted {
        fault: ServiceFault<E>,
    },
    TeardownFaulted {
        fault: ServiceFault<E>,
        runtime: ManuallyDrop<Arc<R>>,
    },
}

/// A read capability over one lazily published generation service.
///
/// The guard keeps the published `Arc` alive without incrementing its strong
/// count on the warm read path.
pub struct GenerationServiceRead<R> {
    guard: arc_swap::Guard<Option<Arc<R>>>,
}

impl<R> Deref for GenerationServiceRead<R> {
    type Target = R;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.guard
            .as_ref()
            .expect("GenerationServiceRead always contains a runtime")
            .as_ref()
    }
}

impl<R> GenerationServiceRead<R> {
    #[inline]
    pub fn as_arc(&self) -> &Arc<R> {
        self.guard
            .as_ref()
            .expect("GenerationServiceRead always contains a runtime")
    }
}

/// Owns an in-flight service initialization.
struct InitializingTxn<'slot, C, R, E: Clone> {
    slot: &'slot GenerationServiceSlot<C, R, E>,
    committed: bool,
}

impl<C, R, E: Clone> InitializingTxn<'_, C, R, E> {
    fn commit(
        mut self,
        runtime: Arc<R>,
        on_initialized: impl FnOnce(&Arc<R>),
    ) -> GenerationServiceRead<R> {
        // Keep the callback outside the state lock. If it panics, Drop owns
        // the transition to InitFaulted.
        on_initialized(&runtime);
        self.slot.published.store(Some(runtime));

        let mut state = self.slot.state.lock();
        match &*state {
            GenerationServiceState::Initializing => {}
            _ => unreachable!("initialization transaction lost its state owner"),
        }
        *state = GenerationServiceState::Ready;
        self.committed = true;
        self.slot.changed.notify_all();
        drop(state);

        let guard = self.slot.published.load();
        debug_assert!(guard.is_some());
        GenerationServiceRead { guard }
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

/// Owns the published runtime while its shutdown callback executes.
struct SealingTxn<'slot, C, R, E: Clone> {
    slot: &'slot GenerationServiceSlot<C, R, E>,
    runtime: Option<Arc<R>>,
    committed: bool,
}

impl<C, R, E: Clone> SealingTxn<'_, C, R, E> {
    fn finish<S>(
        mut self,
        shutdown: impl FnOnce(Arc<R>) -> Result<S, E>,
    ) -> Result<S, ServiceSlotError<E>> {
        let runtime = self
            .runtime
            .as_ref()
            .expect("a sealing transaction owns its runtime root");
        let result = shutdown(Arc::clone(runtime));
        let mut state = self.slot.state.lock();

        match result {
            Ok(sealed) => {
                *state = GenerationServiceState::Closed;
                self.committed = true;
                self.slot.changed.notify_all();
                drop(state);
                self.runtime.take();
                Ok(sealed)
            }
            Err(error) => {
                let runtime = self
                    .runtime
                    .take()
                    .expect("a sealing transaction retains its runtime root");
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
    published: ArcSwapOption<R>,
    state: Mutex<GenerationServiceState<C, R, E>>,
    changed: Condvar,
}

impl<C, R, E> GenerationServiceSlot<C, R, E> {
    pub const fn new() -> Self {
        Self {
            published: ArcSwapOption::const_empty(),
            state: Mutex::new(GenerationServiceState::Closed),
            changed: Condvar::new(),
        }
    }

    pub fn arm(&self, config: C) -> Result<(), ServiceSlotError<E>> {
        let mut state = self.state.lock();
        if !matches!(*state, GenerationServiceState::Closed) {
            return Err(ServiceSlotError::Closed);
        }
        *state = GenerationServiceState::Cold { config };
        self.changed.notify_all();
        Ok(())
    }

    pub fn disarm(&self) -> Result<(), ServiceSlotError<E>> {
        let mut state = self.state.lock();
        match &*state {
            GenerationServiceState::Cold { .. } => {
                *state = GenerationServiceState::Closed;
                self.changed.notify_all();
                Ok(())
            }
            GenerationServiceState::Closed => Ok(()),
            _ => Err(ServiceSlotError::Closed),
        }
    }

    pub fn is_none(&self) -> bool {
        self.published.load().is_none()
            && matches!(
                *self.state.lock(),
                GenerationServiceState::Closed | GenerationServiceState::InitFaulted { .. }
            )
    }

    /// Loads an already published service without turning a cold slot into a
    /// live one.
    pub fn read_if_ready(&self) -> Option<GenerationServiceRead<R>> {
        let guard = self.published.load();
        guard.is_some().then_some(GenerationServiceRead { guard })
    }

    /// Exposes the published read-side projection for adapter instrumentation.
    pub fn with_published(&self, callback: impl FnOnce(Option<&Arc<R>>)) {
        let published = self.published.load();
        callback(published.as_ref());
    }
}

impl<C, R, E> Default for GenerationServiceSlot<C, R, E> {
    fn default() -> Self {
        Self::new()
    }
}

impl<C, R, E: Clone> GenerationServiceSlot<C, R, E> {
    /// Acquires a read guard, lazily initializing the service if this is the
    /// first reader of the armed generation.
    pub fn read(
        &self,
        initialize: impl FnOnce(C) -> Result<Arc<R>, E>,
        on_initialized: impl FnOnce(&Arc<R>),
    ) -> Result<GenerationServiceRead<R>, ServiceSlotError<E>> {
        let guard = self.published.load();
        if guard.is_some() {
            return Ok(GenerationServiceRead { guard });
        }
        drop(guard);
        self.read_slow(initialize, on_initialized)
    }

    #[cold]
    fn read_slow(
        &self,
        initialize: impl FnOnce(C) -> Result<Arc<R>, E>,
        on_initialized: impl FnOnce(&Arc<R>),
    ) -> Result<GenerationServiceRead<R>, ServiceSlotError<E>> {
        let mut initialize = Some(initialize);
        let mut on_initialized = Some(on_initialized);
        let mut state = self.state.lock();

        loop {
            match &*state {
                GenerationServiceState::Ready => {
                    drop(state);
                    let guard = self.published.load();
                    debug_assert!(guard.is_some());
                    return Ok(GenerationServiceRead { guard });
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

    pub fn seal<S>(
        &self,
        missing_runtime_error: E,
        empty: impl FnOnce() -> S,
        shutdown: impl FnOnce(Arc<R>) -> Result<S, E>,
    ) -> Result<S, ServiceSlotError<E>> {
        let runtime = {
            let mut state = self.state.lock();
            while matches!(
                *state,
                GenerationServiceState::Initializing | GenerationServiceState::Sealing
            ) {
                self.changed.wait(&mut state);
            }

            match &*state {
                GenerationServiceState::Ready => {
                    let runtime = self.published.swap(None);
                    *state = GenerationServiceState::Sealing;
                    runtime
                }
                GenerationServiceState::Cold { .. }
                | GenerationServiceState::InitFaulted { .. } => {
                    *state = GenerationServiceState::Closed;
                    self.changed.notify_all();
                    return Ok(empty());
                }
                GenerationServiceState::Closed => return Ok(empty()),
                GenerationServiceState::TeardownFaulted { fault, .. } => {
                    return Err(ServiceSlotError::Fault(fault.clone()));
                }
                GenerationServiceState::Initializing | GenerationServiceState::Sealing => {
                    unreachable!()
                }
            }
        };

        let Some(runtime) = runtime else {
            let mut state = self.state.lock();
            let fault = ServiceFault::Error(missing_runtime_error.clone());
            *state = GenerationServiceState::InitFaulted {
                fault: fault.clone(),
            };
            self.changed.notify_all();
            return Err(ServiceSlotError::Fault(fault));
        };

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
    use super::{GenerationServiceSlot, GenerationServiceState, ServiceFault, ServiceSlotError};
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::Arc;

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
                    Ok(Arc::new(Service))
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
                |_| -> Result<Arc<Service>, TestError> { panic!("injected initializer panic") },
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
            slot.read(|_| Ok(Arc::new(Service)), |_| {}),
            Err(ServiceSlotError::Fault(ServiceFault::Panicked))
        ));
    }

    #[test]
    fn shutdown_panic_retains_runtime_as_teardown_fault() {
        let slot = GenerationServiceSlot::<(), Service, TestError>::new();
        slot.arm(()).expect("service slot can be armed");
        let _read = slot
            .read(|_| Ok(Arc::new(Service)), |_| {})
            .expect("service slot initializes");

        let result = catch_unwind(AssertUnwindSafe(|| {
            let _ = slot.seal(
                TestError("missing runtime"),
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
            slot.seal(TestError("missing runtime"), || (), |_| Ok(())),
            Err(ServiceSlotError::Fault(ServiceFault::Panicked))
        ));
    }
}
