//! Formula-handle lifetime capability exposed to the private Excel transport.
//!
//! The handle subsystem owns formula bindings and their lifetime state. This
//! contract is deliberately defined here rather than in the RTD module so a
//! handle generation does not depend on the generic RTD subscription API.

#[cfg(target_os = "windows")]
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

#[cfg(any(target_os = "windows", test, feature = "refinement"))]
impl FormulaLifetimeGeneration {
    pub(crate) const fn get(self) -> u64 {
        self.0.get()
    }
}

pub(crate) trait FormulaLifetimeBackend: Send + Sync {
    #[cfg(target_os = "windows")]
    fn identity(&self) -> usize;

    fn terminate_all_topics(&self);

    #[cfg(all(target_os = "windows", any(test, feature = "refinement")))]
    fn lifetime_trace(&self) -> Option<crate::shutdown_trace::ShutdownTraceHandle>;

    #[cfg(target_os = "windows")]
    fn claim_lifetime(
        &self,
        lifetime_key: &str,
        generation: FormulaLifetimeGeneration,
    ) -> XllResult<()>;

    #[cfg(target_os = "windows")]
    fn connect_lifetime<'a>(
        &'a self,
        generation: FormulaLifetimeGeneration,
        topic_id: i32,
        lifetime_key: &str,
    ) -> XllResult<Box<dyn FormulaLifetimeConnection + 'a>>;

    #[cfg(target_os = "windows")]
    fn disconnect(&self, generation: FormulaLifetimeGeneration, topic_id: i32);

    #[cfg(target_os = "windows")]
    fn terminate_topics(&self, generation: FormulaLifetimeGeneration);
}

/// One provisional formula-lifetime connection owned by a COM call.
#[cfg(target_os = "windows")]
pub(crate) trait FormulaLifetimeConnection {
    fn token(&self) -> &str;
    fn commit(self: Box<Self>) -> XllResult<()>;
}
