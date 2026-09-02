#![allow(
    unused_imports,
    reason = "diagnostic router facade re-exports items from the parent module for encapsulation"
)]

//! Internal diagnostic routing and worker lifecycle.

pub(crate) use super::{
    DiagnosticShutdownError, DiagnosticsStopped, close_diagnostic_router, reset_diagnostic_router,
    set_diagnostic_sink,
};

#[cfg(any(test, feature = "refinement"))]
pub(crate) use super::connect_trace;

#[cfg(test)]
pub(crate) use super::{DiagnosticsDrained, clear_diagnostic_sink};
