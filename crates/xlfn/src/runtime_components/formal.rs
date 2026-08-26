//! Verification-only runtime composition state.

#[cfg(any(test, feature = "refinement"))]
use std::sync::{Arc, OnceLock};

#[cfg(any(test, feature = "refinement"))]
/// Verification-only state is isolated from operational runtime components.
pub(crate) struct FormalState {
    pub(crate) trace: OnceLock<crate::shutdown_trace::ShutdownTraceHandle>,
    pub(crate) composition: OnceLock<Arc<crate::composition_refinement::CompositionTrace>>,
}

#[cfg(any(test, feature = "refinement"))]
impl FormalState {
    pub(crate) const fn new() -> Self {
        Self {
            trace: OnceLock::new(),
            composition: OnceLock::new(),
        }
    }
}
