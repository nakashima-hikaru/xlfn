//! Verification-only runtime composition state.

#[cfg(any(test, feature = "refinement"))]
use std::sync::{Arc, OnceLock};

#[cfg(any(test, feature = "refinement"))]
/// Verification-only state is isolated from operational runtime components.
pub(crate) struct FormalState {
    pub(crate) ghost: OnceLock<crate::shutdown_refinement::GhostHandle>,
    pub(crate) composition: OnceLock<Arc<crate::composition_refinement::CompositionTrace>>,
}

#[cfg(any(test, feature = "refinement"))]
impl FormalState {
    pub(crate) const fn new() -> Self {
        Self {
            ghost: OnceLock::new(),
            composition: OnceLock::new(),
        }
    }
}
