#![allow(
    unsafe_code,
    reason = "ExecutionLease is the audited temporal lifetime capability for a uniquely owned generation"
)]

use std::num::NonZeroU64;
use std::ptr::NonNull;

/// Identity of one published or staged runtime service generation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct RuntimeGeneration(NonZeroU64);

impl RuntimeGeneration {
    pub(crate) const fn new(raw: u64) -> Option<Self> {
        match NonZeroU64::new(raw) {
            Some(raw) => Some(Self(raw)),
            None => None,
        }
    }

    pub(crate) const fn get(self) -> u64 {
        self.0.get()
    }
}

/// The execution root of one published Add-in generation.
///
/// Generation ownership lives in this module so lifecycle code can move the
/// root between staged, published, and retired states without depending on
/// the runtime composition root.
pub(crate) struct ExecutionGeneration<A: crate::Addin> {
    pub(crate) id: RuntimeGeneration,
    pub(crate) shared_state: A::SharedState,
    pub(crate) layers: A::Layers,
}

impl<A: crate::Addin> ExecutionGeneration<A> {
    pub(crate) const fn id(&self) -> RuntimeGeneration {
        self.id
    }
}

/// Unique Add-in state staged during `OPENING`.
pub(crate) struct OpeningGeneration<A: crate::Addin> {
    pub(crate) shared_state: A::SharedState,
    pub(crate) layers: A::Layers,
    pub(crate) init_config: crate::addin::RuntimeConfig,
}

impl<A: crate::Addin> OpeningGeneration<A> {
    #[must_use]
    pub(crate) fn into_parts(self) -> (A::SharedState, A::Layers, crate::addin::RuntimeConfig) {
        (self.shared_state, self.layers, self.init_config)
    }
}

/// Generation reclaimed during shutdown.
pub(crate) enum ShutdownGeneration<A: crate::Addin> {
    Opening(OpeningGeneration<A>),
    Open(Box<ExecutionGeneration<A>>),
}

/// Explicit open-generation lifetime lease for call-scoped and asynchronous
/// UDF executions.
pub struct ExecutionLease<A: crate::Addin> {
    generation: NonNull<ExecutionGeneration<A>>,
    _permit: xlfn_kernel::drain_gate::OwnedDrainPermit,
}

impl<A: crate::Addin> ExecutionLease<A> {
    /// Creates a temporal lease over the uniquely owned generation root.
    ///
    /// # Safety
    ///
    /// `generation` must remain valid until `permit` is dropped, and the gate
    /// that issued `permit` must be drained before the generation is reclaimed.
    #[cfg(feature = "async")]
    pub(crate) unsafe fn new(
        generation: NonNull<ExecutionGeneration<A>>,
        permit: xlfn_kernel::drain_gate::OwnedDrainPermit,
    ) -> Self {
        Self {
            generation,
            _permit: permit,
        }
    }

    #[must_use]
    pub fn state(&self) -> &A::SharedState {
        // SAFETY: construction couples the pointer to the owned drain permit.
        &unsafe { self.generation.as_ref() }.shared_state
    }
}

// SAFETY: the generation payload is Send + Sync by the Addin contract and the
// owned permit prevents reclamation while the pointer crosses threads.
unsafe impl<A: crate::Addin> Send for ExecutionLease<A> {}
// SAFETY: same invariant as Send; shared borrows access immutable shared_state.
unsafe impl<A: crate::Addin> Sync for ExecutionLease<A> {}

/// Identity of an in-flight open transaction. It is distinct from the
/// published generation even though both participate in lifecycle state.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct OpenAttemptId(NonZeroU64);

impl OpenAttemptId {
    pub(crate) const fn new(raw: u64) -> Option<Self> {
        match NonZeroU64::new(raw) {
            Some(raw) => Some(Self(raw)),
            None => None,
        }
    }

    pub(crate) const fn get(self) -> u64 {
        self.0.get()
    }

    /// Promote the identity of a successful open transaction to the
    /// published-generation identity. This is the only conversion between
    /// the two lifecycle domains; callers must not pass either identity
    /// through an untyped integer at the promotion boundary.
    pub(crate) const fn into_runtime_generation(self) -> RuntimeGeneration {
        RuntimeGeneration(self.0)
    }
}

/// Monotonic close epoch used to invalidate open attempts sampled before a
/// final removal acquired its owner.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct RemovalEpoch(u64);

impl RemovalEpoch {
    pub(crate) const fn new(raw: u64) -> Self {
        Self(raw)
    }

    pub(crate) const fn get(self) -> u64 {
        self.0
    }
}

/// Identity of one owner of the terminal removal protocol.
///
/// A removal request may advance the close epoch more than once while callers
/// wait for an earlier owner to leave. This identity is therefore separate
/// from [`RemovalEpoch`]: it names the affine owner that is allowed to issue a
/// terminal certificate.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct RemovalAttemptId(NonZeroU64);

impl RemovalAttemptId {
    pub(crate) const fn new(raw: u64) -> Option<Self> {
        match NonZeroU64::new(raw) {
            Some(raw) => Some(Self(raw)),
            None => None,
        }
    }
}

/// Identity of a binding slot incarnation.  A slot can be reused only with
/// the next value, so zero is never a valid binding generation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct BindingGeneration(NonZeroU64);

impl BindingGeneration {
    pub(crate) const ONE: Self = Self(NonZeroU64::MIN);

    pub(crate) const fn new(raw: u64) -> Option<Self> {
        match NonZeroU64::new(raw) {
            Some(raw) => Some(Self(raw)),
            None => None,
        }
    }

    pub(crate) const fn get(self) -> u64 {
        self.0.get()
    }

    pub(crate) const fn next(self) -> Option<Self> {
        match self.0.get().checked_add(1) {
            Some(raw) => Self::new(raw),
            None => None,
        }
    }
}

/// Identity of one mutable topic-table incarnation.  It is separate from
/// the runtime generation because a topic table may reject an initializer
/// without creating a new runtime.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct TopicGeneration(NonZeroU64);

impl TopicGeneration {
    pub(crate) const ONE: Self = Self(NonZeroU64::MIN);

    pub(crate) const fn next(self) -> Option<Self> {
        match self.0.get().checked_add(1) {
            Some(raw) => Self::new(raw),
            None => None,
        }
    }

    const fn new(raw: u64) -> Option<Self> {
        match NonZeroU64::new(raw) {
            Some(raw) => Some(Self(raw)),
            None => None,
        }
    }
}

/// Identity of one COM/RTD server instance. The zero sentinel is reserved for
/// the absence of an active server and never enters subscription maps.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct ServerGeneration(NonZeroU64);

#[allow(
    dead_code,
    reason = "constructed at the Windows RTD boundary and in tests"
)]
impl ServerGeneration {
    pub(crate) const fn new(raw: u64) -> Option<Self> {
        match NonZeroU64::new(raw) {
            Some(raw) => Some(Self(raw)),
            None => None,
        }
    }

    pub(crate) const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Monotonic identity of one subscription connection attempt.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct ConnectionGeneration(NonZeroU64);

impl ConnectionGeneration {
    pub(crate) const fn new(raw: u64) -> Option<Self> {
        match NonZeroU64::new(raw) {
            Some(raw) => Some(Self(raw)),
            None => None,
        }
    }
}
