//! Quarantine ownership for resources whose unload safety is not proven.

use crate::sync::Mutex;
use std::mem::ManuallyDrop;

use crate::generation::ExecutionGeneration;
use crate::generation::RuntimeGeneration;

/// A terminal reason for retaining a resource instead of running its
/// destructor after unload safety could not be established.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum QuarantineReason {
    OpenStateInvariant,
    AddinQuiesceFailed,
    AddinCleanupPanicked,
    TeardownIncomplete,
}

/// Explicit ownership for resources that are intentionally never dropped
/// after a quarantine decision. `ManuallyDrop` is used as documentation and
/// as a type-level guarantee that dropping the runtime cannot accidentally
/// execute code whose quiescence was not proven.
#[expect(
    dead_code,
    reason = "quarantined payloads are intentionally retained without reading or dropping"
)]
enum QuarantinedPayload<A: crate::Addin> {
    ModuleAuthority(crate::module_runtime::ModuleAuthority),
    SharedState(A::SharedState),
    Layers(A::Layers),
    Generation(A::SharedState, A::Layers),
}

struct QuarantinedRecord<A: crate::Addin> {
    generation: Option<RuntimeGeneration>,
    reason: QuarantineReason,
    _payload: ManuallyDrop<QuarantinedPayload<A>>,
}

pub(crate) struct QuarantineVault<A: crate::Addin> {
    resources: Mutex<Vec<QuarantinedRecord<A>>>,
}

impl<A: crate::Addin> QuarantineVault<A> {
    pub(crate) const fn new() -> Self {
        Self {
            resources: Mutex::new(Vec::new()),
        }
    }

    fn push(
        &self,
        generation: Option<RuntimeGeneration>,
        reason: QuarantineReason,
        payload: QuarantinedPayload<A>,
    ) {
        self.resources.lock().push(QuarantinedRecord {
            generation,
            reason,
            _payload: ManuallyDrop::new(payload),
        });
    }

    /// A rejected canonical installation is not cleanup success. Keep its
    /// affine authority without retrying drain or releasing its module epoch.
    pub(crate) fn retain_module_authority(
        &self,
        authority: crate::module_runtime::ModuleAuthority,
    ) {
        self.push(
            None,
            QuarantineReason::OpenStateInvariant,
            QuarantinedPayload::ModuleAuthority(authority),
        );
    }

    pub(crate) fn retain_shared_state(
        &self,
        generation: Option<RuntimeGeneration>,
        shared_state: A::SharedState,
        reason: QuarantineReason,
    ) {
        self.push(
            generation,
            reason,
            QuarantinedPayload::SharedState(shared_state),
        );
    }

    pub(crate) fn retain_layers(
        &self,
        generation: Option<RuntimeGeneration>,
        layers: A::Layers,
        reason: QuarantineReason,
    ) {
        self.push(generation, reason, QuarantinedPayload::Layers(layers));
    }

    pub(crate) fn retain_generation(
        &self,
        generation: Option<RuntimeGeneration>,
        root: ExecutionGeneration<A>,
        reason: QuarantineReason,
    ) {
        let ExecutionGeneration {
            shared_state,
            layers,
            ..
        } = root;
        self.push(
            generation,
            reason,
            QuarantinedPayload::Generation(shared_state, layers),
        );
    }

    pub(crate) fn snapshot(&self) -> Vec<(Option<RuntimeGeneration>, QuarantineReason)> {
        self.resources
            .lock()
            .iter()
            .map(|record| (record.generation, record.reason))
            .collect()
    }
}
