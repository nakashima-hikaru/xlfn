//! Formula-handle lifetime identity and concrete private transport types.
//!
//! The handle subsystem owns formula bindings and their lifetime state. This
//! boundary names its concrete implementation only when handles are enabled;
//! RTD-only builds do not compile formula-handle state or token machinery.

#[cfg(all(target_os = "windows", not(feature = "handles")))]
use crate::XllResult;
use std::num::NonZeroU64;

/// Identity of one formula-lifetime observer generation.
///
/// The Windows adapter converts its COM server generation into this semantic
/// handle-side identity at the private transport boundary.  Handle state does
/// not depend on the transport's generation type.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct FormulaLifetimeGeneration(NonZeroU64);

#[cfg(any(target_os = "windows", test))]
impl FormulaLifetimeGeneration {
    pub(crate) const fn new(raw: u64) -> Option<Self> {
        match NonZeroU64::new(raw) {
            Some(raw) => Some(Self(raw)),
            None => None,
        }
    }
}

#[cfg(all(feature = "handles", any(test, feature = "refinement")))]
impl FormulaLifetimeGeneration {
    pub(crate) const fn get(self) -> u64 {
        self.0.get()
    }
}

// The transport has exactly one formula-handle implementation. Keep its
// concrete types behind this private boundary; RTD-only builds use uninhabited
// placeholders and do not compile any formula-handle state.
#[cfg(feature = "handles")]
pub(crate) use crate::handle::FormulaHandleService as FormulaLifetimeBackend;

#[cfg(all(target_os = "windows", not(feature = "handles")))]
pub(crate) enum FormulaLifetimeBackend {}

#[cfg(all(target_os = "windows", not(feature = "handles")))]
impl FormulaLifetimeBackend {
    pub(crate) fn identity(&self) -> usize {
        match *self {}
    }

    pub(crate) fn connect_transaction(
        &self,
        _: FormulaLifetimeGeneration,
        _: i32,
        _: &str,
    ) -> XllResult<FormulaLifetimeConnection<'_>> {
        match *self {}
    }

    pub(crate) fn disconnect(&self, _: FormulaLifetimeGeneration, _: i32) {
        match *self {}
    }

    pub(crate) fn terminate_topics(&self, _: FormulaLifetimeGeneration) {
        match *self {}
    }
}

#[cfg(all(target_os = "windows", not(feature = "handles")))]
pub(crate) struct FormulaLifetimeConnection<'runtime> {
    never: std::convert::Infallible,
    _runtime: std::marker::PhantomData<&'runtime ()>,
}

#[cfg(all(target_os = "windows", not(feature = "handles")))]
impl FormulaLifetimeConnection<'_> {
    pub(crate) fn token(&self) -> &str {
        match self.never {}
    }

    pub(crate) fn commit(self) -> XllResult<()> {
        match self.never {}
    }
}
