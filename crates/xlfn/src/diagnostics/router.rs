#![allow(
    unused_imports,
    reason = "module boundary reexports are consumed through their parent"
)]

//! Internal diagnostic routing and worker lifecycle.

pub(crate) use super::{
    DiagnosticShutdownError, DiagnosticsStopped, close_diagnostic_router, reset_diagnostic_router,
    set_diagnostic_sink,
};

#[cfg(any(test, feature = "refinement"))]
pub(crate) use super::connect_ghost;

#[cfg(test)]
pub(crate) use super::{DiagnosticsDrained, clear_diagnostic_sink};
