//! Shared generation-scoped service slot protocol.

use arc_swap::ArcSwapOption;
use parking_lot::{Condvar, Mutex};
use std::mem::ManuallyDrop;
use std::ops::Deref;
use std::sync::Arc;

use crate::generation::RuntimeGeneration;

/// Shared lifecycle vocabulary for generation-scoped lazy services. The
/// service modules keep their own initialization and teardown policy, while
/// this state machine prevents their public phase vocabulary from diverging.
pub(crate) enum GenerationServiceState<C, T> {
    Closed,
    Cold {
        generation: RuntimeGeneration,
        config: C,
    },
    Initializing {
        generation: RuntimeGeneration,
    },
    Ready {
        generation: RuntimeGeneration,
    },
    Sealing {
        generation: RuntimeGeneration,
    },
    InitFaulted {
        generation: RuntimeGeneration,
        error: crate::XllError,
    },
    TeardownFaulted {
        generation: RuntimeGeneration,
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

    pub(crate) fn arm(&self, generation: RuntimeGeneration, config: C) -> crate::XllResult<()> {
        let mut state = self.state.lock();
        if !matches!(*state, GenerationServiceState::Closed) {
            return Err(crate::XllError::Closing);
        }
        *state = GenerationServiceState::Cold { generation, config };
        self.changed.notify_all();
        Ok(())
    }

    pub(crate) fn disarm(&self, generation: RuntimeGeneration) -> crate::XllResult<()> {
        let mut state = self.state.lock();
        match &*state {
            GenerationServiceState::Cold {
                generation: active, ..
            } if *active == generation => {
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

    #[cfg(any(test, feature = "shutdown-refinement"))]
    pub(crate) fn with_published(&self, callback: impl FnOnce(Option<&Arc<R>>)) {
        let published = self.published.load();
        callback(published.as_ref());
    }
}

impl<C: Copy, R> GenerationServiceSlot<C, R> {
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
                GenerationServiceState::Ready { .. } => {
                    drop(state);
                    let guard = self.published.load();
                    debug_assert!(guard.is_some());
                    return Ok(GenerationServiceRead { guard });
                }
                GenerationServiceState::InitFaulted { error, .. }
                | GenerationServiceState::TeardownFaulted { error, .. } => {
                    return Err(error.clone());
                }
                GenerationServiceState::Initializing { generation }
                | GenerationServiceState::Sealing { generation } => {
                    let _ = generation;
                    self.changed.wait(&mut state);
                }
                GenerationServiceState::Cold { generation, config } => {
                    let generation = *generation;
                    let config = *config;
                    *state = GenerationServiceState::Initializing { generation };
                    drop(state);

                    let candidate = initialize
                        .take()
                        .expect("a service initializer is consumed exactly once")(
                        config
                    );

                    let mut state = self.state.lock();
                    match candidate {
                        Ok(runtime) => {
                            if let Some(on_initialized) = on_initialized.take() {
                                on_initialized(&runtime);
                            }
                            self.published.store(Some(runtime));
                            *state = GenerationServiceState::Ready { generation };
                            self.changed.notify_all();
                            drop(state);

                            let guard = self.published.load();
                            debug_assert!(guard.is_some());
                            return Ok(GenerationServiceRead { guard });
                        }
                        Err(error) => {
                            *state = GenerationServiceState::InitFaulted {
                                generation,
                                error: error.clone(),
                            };
                            self.changed.notify_all();
                            return Err(error);
                        }
                    }
                }
                GenerationServiceState::Closed => return Err(crate::XllError::Closing),
            }
        }
    }

    pub(crate) fn seal<S>(
        &self,
        generation: Option<RuntimeGeneration>,
        missing_runtime_diagnostic: crate::error::DiagnosticId,
        empty: impl FnOnce() -> S,
        shutdown: impl FnOnce(Arc<R>) -> crate::XllResult<S>,
    ) -> crate::XllResult<S> {
        let runtime = {
            let mut state = self.state.lock();
            while matches!(
                *state,
                GenerationServiceState::Initializing { .. }
                    | GenerationServiceState::Sealing { .. }
            ) {
                self.changed.wait(&mut state);
            }

            match &*state {
                GenerationServiceState::Ready { generation: active } => {
                    if generation != Some(*active) {
                        return Err(crate::XllError::Closing);
                    }
                    let runtime = self.published.swap(None);
                    *state = GenerationServiceState::Sealing {
                        generation: *active,
                    };
                    runtime
                }
                GenerationServiceState::Cold {
                    generation: active, ..
                }
                | GenerationServiceState::InitFaulted {
                    generation: active, ..
                } => {
                    if generation != Some(*active) {
                        return Err(crate::XllError::Closing);
                    }
                    *state = GenerationServiceState::Closed;
                    self.changed.notify_all();
                    return Ok(empty());
                }
                GenerationServiceState::Closed => return Ok(empty()),
                GenerationServiceState::TeardownFaulted {
                    generation: active,
                    error,
                    runtime,
                } => {
                    let _ = runtime;
                    if generation != Some(*active) {
                        return Err(crate::XllError::Closing);
                    }
                    return Err(error.clone());
                }
                GenerationServiceState::Initializing { .. }
                | GenerationServiceState::Sealing { .. } => unreachable!(),
            }
        };

        let Some(runtime) = runtime else {
            return Err(crate::XllError::Internal {
                diagnostic_id: missing_runtime_diagnostic,
            });
        };
        let result = shutdown(Arc::clone(&runtime));
        let mut state = self.state.lock();
        match result {
            Ok(sealed) => {
                *state = GenerationServiceState::Closed;
                self.changed.notify_all();
                Ok(sealed)
            }
            Err(error) => {
                *state = GenerationServiceState::TeardownFaulted {
                    generation: generation.expect("a live service runtime has a generation"),
                    error: error.clone(),
                    runtime: ManuallyDrop::new(runtime),
                };
                self.changed.notify_all();
                Err(error)
            }
        }
    }
}
