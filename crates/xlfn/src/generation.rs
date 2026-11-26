#[cfg(feature = "handles")]
use std::num::NonZeroU64;
#[cfg(any(feature = "rtd", feature = "handles"))]
pub(crate) use xlfn_engine::generation::{ConnectionGeneration, ServerGeneration};
pub(crate) use xlfn_engine::generation::{
    OpenAttemptId, RemovalAttemptId, RemovalEpoch, RuntimeGeneration,
};

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
    pub(crate) init_config: crate::addin_impl::RuntimeConfig,
}

impl<A: crate::Addin> OpeningGeneration<A> {
    #[must_use]
    pub(crate) fn into_parts(
        self,
    ) -> (A::SharedState, A::Layers, crate::addin_impl::RuntimeConfig) {
        (self.shared_state, self.layers, self.init_config)
    }
}

/// Generation reclaimed during shutdown.
pub(crate) enum ShutdownGeneration<A: crate::Addin> {
    Opening(OpeningGeneration<A>),
    Open(Box<ExecutionGeneration<A>>),
}

// Construction remains private to the publication/admission issuer module.
pub use crate::lifecycle::ExecutionLease;

/// Identity of a binding slot incarnation.  A slot can be reused only with
/// the next value, so zero is never a valid binding generation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[cfg(feature = "handles")]
pub(crate) struct BindingGeneration(NonZeroU64);

#[cfg(feature = "handles")]
impl BindingGeneration {
    pub(crate) const ONE: Self = Self(NonZeroU64::MIN);

    pub(crate) const fn try_new(raw: u64) -> Option<Self> {
        match NonZeroU64::new(raw) {
            Some(raw) => Some(Self(raw)),
            None => None,
        }
    }

    pub(crate) const fn get(self) -> NonZeroU64 {
        self.0
    }

    pub(crate) const fn next(self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(raw) => Some(Self(raw)),
            None => None,
        }
    }
}

/// Identity of one mutable topic-table incarnation.  It is separate from
/// the runtime generation because a topic table may reject an initializer
/// without creating a new runtime.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[cfg(feature = "handles")]
pub(crate) struct TopicGeneration(NonZeroU64);

#[cfg(feature = "handles")]
impl TopicGeneration {
    pub(crate) const ONE: Self = Self(NonZeroU64::MIN);

    pub(crate) const fn next(self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(raw) => Some(Self(raw)),
            None => None,
        }
    }
}
