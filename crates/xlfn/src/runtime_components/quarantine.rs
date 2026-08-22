//! Quarantine ownership for resources whose unload safety is not proven.

use parking_lot::Mutex;
use std::mem::ManuallyDrop;
use std::sync::Arc;

use crate::generation::RuntimeGeneration;
use crate::runtime::OpenGeneration;

/// A terminal reason for retaining a resource instead of running its
/// destructor after unload safety could not be established.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum QuarantineReason {
    OpenStateInvariant,
    AddinQuiesceFailed,
    AddinGenerationEscaped,
    AddinCleanupPanicked,
    BoundaryFailure,
}

/// Explicit ownership for resources that are intentionally never dropped
/// after a quarantine decision. `ManuallyDrop` is used as documentation and
/// as a type-level guarantee that dropping the runtime cannot accidentally
/// execute code whose quiescence was not proven.
pub(crate) enum QuarantinedResource<A: crate::Addin> {
    SharedState {
        generation: Option<RuntimeGeneration>,
        shared_state: ManuallyDrop<A::SharedState>,
        reason: QuarantineReason,
    },
    Layers {
        generation: Option<RuntimeGeneration>,
        layers: ManuallyDrop<A::Layers>,
        reason: QuarantineReason,
    },
    Generation {
        generation: Option<RuntimeGeneration>,
        shared_state: ManuallyDrop<A::SharedState>,
        layers: ManuallyDrop<A::Layers>,
        reason: QuarantineReason,
    },
    SharedGeneration {
        generation: Option<RuntimeGeneration>,
        generation_root: ManuallyDrop<Arc<OpenGeneration<A>>>,
        reason: QuarantineReason,
    },
}

pub(crate) struct QuarantineVault<A: crate::Addin> {
    resources: Mutex<Vec<QuarantinedResource<A>>>,
}

impl<A: crate::Addin> QuarantineVault<A> {
    pub(crate) const fn new() -> Self {
        Self {
            resources: Mutex::new(Vec::new()),
        }
    }

    pub(crate) fn retain_shared_state(
        &self,
        generation: Option<RuntimeGeneration>,
        shared_state: A::SharedState,
        reason: QuarantineReason,
    ) {
        self.resources
            .lock()
            .push(QuarantinedResource::SharedState {
                generation,
                shared_state: ManuallyDrop::new(shared_state),
                reason,
            });
    }

    pub(crate) fn retain_layers(
        &self,
        generation: Option<RuntimeGeneration>,
        layers: A::Layers,
        reason: QuarantineReason,
    ) {
        self.resources.lock().push(QuarantinedResource::Layers {
            generation,
            layers: ManuallyDrop::new(layers),
            reason,
        });
    }

    pub(crate) fn retain_generation(
        &self,
        generation: Option<RuntimeGeneration>,
        root: OpenGeneration<A>,
        reason: QuarantineReason,
    ) {
        let OpenGeneration {
            shared_state,
            layers,
            ..
        } = root;
        self.resources.lock().push(QuarantinedResource::Generation {
            generation,
            shared_state: ManuallyDrop::new(shared_state),
            layers: ManuallyDrop::new(layers),
            reason,
        });
    }

    pub(crate) fn retain_shared_generation(
        &self,
        generation: Option<RuntimeGeneration>,
        root: Arc<OpenGeneration<A>>,
        reason: QuarantineReason,
    ) {
        self.resources
            .lock()
            .push(QuarantinedResource::SharedGeneration {
                generation,
                generation_root: ManuallyDrop::new(root),
                reason,
            });
    }

    pub(crate) fn snapshot(&self) -> Vec<(Option<RuntimeGeneration>, QuarantineReason)> {
        self.resources
            .lock()
            .iter()
            .map(|resource| match resource {
                QuarantinedResource::SharedState {
                    generation,
                    shared_state,
                    reason,
                } => {
                    let _ = shared_state;
                    (*generation, *reason)
                }
                QuarantinedResource::Layers {
                    generation,
                    layers,
                    reason,
                } => {
                    let _ = layers;
                    (*generation, *reason)
                }
                QuarantinedResource::Generation {
                    generation,
                    shared_state,
                    layers,
                    reason,
                } => {
                    let _ = (shared_state, layers);
                    (*generation, *reason)
                }
                QuarantinedResource::SharedGeneration {
                    generation,
                    generation_root,
                    reason,
                } => {
                    let _ = generation_root;
                    (*generation, *reason)
                }
            })
            .collect()
    }
}
