//! Shared generation-scoped service slot protocol.

use arc_swap::ArcSwapOption;
use parking_lot::{Condvar, Mutex};
use std::mem::ManuallyDrop;
use std::ops::Deref;
use std::sync::Arc;

/// Shared lifecycle vocabulary for generation-scoped lazy services. The
/// service modules keep their own initialization and teardown policy, while
/// this state machine prevents their public phase vocabulary from diverging.
pub(crate) enum GenerationServiceState<C, T> {
    Closed,
    Cold {
        config: C,
    },
    Initializing,
    Ready,
    Sealing,
    InitFaulted {
        error: crate::XllError,
    },
    TeardownFaulted {
        error: crate::XllError,
        runtime: ManuallyDrop<Arc<T>>,
    },
}

/// A read capability over one lazily published generation service.
///
/// The guard keeps the published `Arc` alive without incrementing its strong
/// count on the warm read path. Service modules expose this concrete read
/// capability under their own names, but the ownership and publication
/// protocol remains shared.
pub(crate) struct GenerationServiceRead<R> {
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
    pub(crate) fn as_arc(&self) -> &Arc<R> {
        self.guard
            .as_ref()
            .expect("GenerationServiceRead always contains a runtime")
    }
}

/// Owns an in-flight service initialization.
///
/// Dropping an uncommitted transaction records a panic fault and wakes every
/// waiter.  An initializer can therefore never leave the slot permanently in
/// `Initializing` when it unwinds.
struct InitializingTxn<'slot, C, R> {
    slot: &'slot GenerationServiceSlot<C, R>,
    committed: bool,
}

impl<C, R> InitializingTxn<'_, C, R> {
    fn commit(
        mut self,
        runtime: Arc<R>,
        on_initialized: impl FnOnce(&Arc<R>),
    ) -> GenerationServiceRead<R> {
        // Keep the callback outside the state lock.  If it panics, Drop owns
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

    fn fail(mut self, error: crate::XllError) -> crate::XllError {
        self.record_fault(error.clone());
        self.committed = true;
        error
    }

    fn record_fault(&mut self, error: crate::XllError) {
        let mut state = self.slot.state.lock();
        if matches!(&*state, GenerationServiceState::Initializing) {
            *state = GenerationServiceState::InitFaulted { error };
            self.slot.changed.notify_all();
        }
    }
}

impl<C, R> Drop for InitializingTxn<'_, C, R> {
    fn drop(&mut self) {
        if !self.committed {
            self.record_fault(crate::XllError::Panic);
        }
    }
}

/// Owns the published runtime while its shutdown callback executes.
///
/// The runtime root remains in this transaction until shutdown commits.  An
/// unwind transfers it to `TeardownFaulted`, so the fault path retains the
/// same resource root as an ordinary shutdown error.
struct SealingTxn<'slot, C, R> {
    slot: &'slot GenerationServiceSlot<C, R>,
    runtime: Option<Arc<R>>,
    committed: bool,
}

impl<C, R> SealingTxn<'_, C, R> {
    fn finish<S>(
        mut self,
        shutdown: impl FnOnce(Arc<R>) -> crate::XllResult<S>,
    ) -> crate::XllResult<S> {
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
                    error: error.clone(),
                    runtime: ManuallyDrop::new(runtime),
                };
                self.committed = true;
                self.slot.changed.notify_all();
                Err(error)
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
                error: crate::XllError::Panic,
                runtime: ManuallyDrop::new(runtime),
            };
            self.slot.changed.notify_all();
        }
    }
}

impl<C, R> Drop for SealingTxn<'_, C, R> {
    fn drop(&mut self) {
        if !self.committed {
            self.record_fault();
        }
    }
}

/// Common state-machine kernel for a generation-scoped lazy service.
///
/// The service-specific modules provide initialization and shutdown closures,
/// while this type owns every transition involving `Closed`, `Cold`,
/// `Initializing`, `Ready`, `Sealing`, and fault retention. In particular,
/// both handle and subscription services use the same wait protocol for a
/// concurrent initializer or sealer.
pub(crate) struct GenerationServiceSlot<C, R> {
    published: ArcSwapOption<R>,
    state: Mutex<GenerationServiceState<C, R>>,
    changed: Condvar,
}

impl<C, R> GenerationServiceSlot<C, R> {
    pub(crate) const fn new() -> Self {
        Self {
            published: ArcSwapOption::const_empty(),
            state: Mutex::new(GenerationServiceState::Closed),
            changed: Condvar::new(),
        }
    }

    pub(crate) fn arm(&self, config: C) -> crate::XllResult<()> {
        let mut state = self.state.lock();
        if !matches!(*state, GenerationServiceState::Closed) {
            return Err(crate::XllError::Closing);
        }
        *state = GenerationServiceState::Cold { config };
        self.changed.notify_all();
        Ok(())
    }

    pub(crate) fn disarm(&self) -> crate::XllResult<()> {
        let mut state = self.state.lock();
        match &*state {
            GenerationServiceState::Cold { .. } => {
                *state = GenerationServiceState::Closed;
                self.changed.notify_all();
                Ok(())
            }
            GenerationServiceState::Closed => Ok(()),
            _ => Err(crate::XllError::Closing),
        }
    }

    pub(crate) fn is_none(&self) -> bool {
        self.published.load().is_none()
            && matches!(
                *self.state.lock(),
                GenerationServiceState::Closed | GenerationServiceState::InitFaulted { .. }
            )
    }

    #[cfg(any(test, feature = "unstable"))]
    pub(crate) fn with_published(&self, callback: impl FnOnce(Option<&Arc<R>>)) {
        let published = self.published.load();
        callback(published.as_ref());
    }
}

impl<C, R> GenerationServiceSlot<C, R> {
    /// Acquires a read guard, lazily initializing the service if this is the
    /// first reader of the armed generation.
    pub(crate) fn read(
        &self,
        initialize: impl FnOnce(C) -> crate::XllResult<Arc<R>>,
        on_initialized: impl FnOnce(&Arc<R>),
    ) -> crate::XllResult<GenerationServiceRead<R>> {
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
        initialize: impl FnOnce(C) -> crate::XllResult<Arc<R>>,
        on_initialized: impl FnOnce(&Arc<R>),
    ) -> crate::XllResult<GenerationServiceRead<R>> {
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
                GenerationServiceState::InitFaulted { error }
                | GenerationServiceState::TeardownFaulted { error, .. } => {
                    return Err(error.clone());
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
                        Err(error) => {
                            return Err(transaction.fail(error));
                        }
                    }
                }
                GenerationServiceState::Closed => return Err(crate::XllError::Closing),
            }
        }
    }

    pub(crate) fn seal<S>(
        &self,
        missing_runtime_diagnostic: crate::error::DiagnosticId,
        empty: impl FnOnce() -> S,
        shutdown: impl FnOnce(Arc<R>) -> crate::XllResult<S>,
    ) -> crate::XllResult<S> {
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
                GenerationServiceState::TeardownFaulted { error, runtime } => {
                    let _ = runtime;
                    return Err(error.clone());
                }
                GenerationServiceState::Initializing | GenerationServiceState::Sealing => {
                    unreachable!()
                }
            }
        };

        let Some(runtime) = runtime else {
            let error = crate::XllError::Internal {
                diagnostic_id: missing_runtime_diagnostic,
            };
            let mut state = self.state.lock();
            *state = GenerationServiceState::InitFaulted {
                error: error.clone(),
            };
            self.changed.notify_all();
            return Err(error);
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
    use super::{GenerationServiceSlot, GenerationServiceState};
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::Arc;

    struct NonCopyConfig(String);
    struct Service;

    #[test]
    fn initialization_moves_a_non_copy_config_once() {
        let slot = GenerationServiceSlot::<NonCopyConfig, Service>::new();
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
        let slot = GenerationServiceSlot::<(), Service>::new();
        slot.arm(()).expect("service slot can be armed");

        let result = catch_unwind(AssertUnwindSafe(|| {
            let _ = slot.read(
                |_| -> crate::XllResult<Arc<Service>> { panic!("injected initializer panic") },
                |_| {},
            );
        }));
        assert!(result.is_err());
        assert!(matches!(
            &*slot.state.lock(),
            GenerationServiceState::InitFaulted {
                error: crate::XllError::Panic,
            }
        ));
        assert!(matches!(
            slot.read(|_| Ok(Arc::new(Service)), |_| {}),
            Err(crate::XllError::Panic)
        ));
    }

    #[test]
    fn shutdown_panic_retains_runtime_as_teardown_fault() {
        let slot = GenerationServiceSlot::<(), Service>::new();
        slot.arm(()).expect("service slot can be armed");
        let _read = slot
            .read(|_| Ok(Arc::new(Service)), |_| {})
            .expect("service slot initializes");

        let result = catch_unwind(AssertUnwindSafe(|| {
            let _ = slot.seal(
                crate::error::DiagnosticId::HANDLE_SLOT,
                || (),
                |_| -> crate::XllResult<()> { panic!("injected shutdown panic") },
            );
        }));
        assert!(result.is_err());
        assert!(matches!(
            &*slot.state.lock(),
            GenerationServiceState::TeardownFaulted {
                error: crate::XllError::Panic,
                ..
            }
        ));
        assert!(matches!(
            slot.seal(
                crate::error::DiagnosticId::HANDLE_SLOT,
                || (),
                |_| { Ok(()) }
            ),
            Err(crate::XllError::Panic)
        ));
    }
}
