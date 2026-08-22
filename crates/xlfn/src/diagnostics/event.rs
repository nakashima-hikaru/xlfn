//! Stable diagnostic event and sink surface.

use crate::XllError;
use crate::error::DiagnosticId;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

/// Receives detailed failures while Excel continues to receive only safe error values.
pub trait DiagnosticSink: Send + 'static {
    /// Records one event and returns in bounded time.
    fn report(&self, event: &DiagnosticEvent<'_>);
}

/// A single failed framework or UDF invocation.
#[non_exhaustive]
pub struct DiagnosticEvent<'a> {
    pub(super) udf_id: &'static str,
    pub(super) argument: Option<&'static str>,
    pub(super) error: &'a XllError,
    pub(super) diagnostic_id: DiagnosticId,
    pub(super) timestamp: SystemTime,
}

impl DiagnosticEvent<'_> {
    #[must_use]
    pub const fn udf_id(&self) -> &'static str {
        self.udf_id
    }

    #[must_use]
    pub const fn argument(&self) -> Option<&'static str> {
        self.argument
    }

    #[must_use]
    pub const fn error(&self) -> &XllError {
        self.error
    }

    #[must_use]
    pub const fn diagnostic_id(&self) -> DiagnosticId {
        self.diagnostic_id
    }

    #[must_use]
    pub const fn timestamp(&self) -> SystemTime {
        self.timestamp
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DiagnosticInitError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("failed to start diagnostic logger worker: {0}")]
    WorkerSpawn(#[source] io::Error),
    #[error("diagnostic sink mutation was requested from its own worker")]
    ReentrantMutation,
    #[error("the diagnostic router is closing or closed")]
    RouterClosed,
}

#[allow(
    dead_code,
    reason = "Shutdown error is retained for internal lifecycle diagnostics"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum DiagnosticShutdownError {
    #[error("diagnostic logger worker panicked")]
    WorkerPanicked,
    #[error("diagnostic logger cannot join itself")]
    ReentrantShutdown,
    #[error("the diagnostic router is closed")]
    RouterClosed,
    #[error("diagnostic router invariant violated")]
    InvariantViolation,
}

/// Stable identifier used to scope an add-in's diagnostic log and metadata.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AddinId(String);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid addin id")]
pub struct InvalidAddinId;

impl AddinId {
    pub fn parse(value: &str) -> Result<Self, InvalidAddinId> {
        if value.len() > 64 || value.starts_with('.') {
            return Err(InvalidAddinId);
        }
        if xlfn_common::validate_windows_basename(value).is_err() {
            return Err(InvalidAddinId);
        }

        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
#[derive(Debug)]
pub struct DiagnosticsDrained {
    pub(super) _private: (),
}

/// Operational metrics snapshot for the diagnostic subsystem.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct DiagnosticStats {
    /// Number of diagnostic events dropped because the bounded queue was full or closed.
    pub dropped_events: u64,
    /// Number of file diagnostic deliveries that failed during write or rotation.
    pub file_write_failures: u64,
}

pub(super) static DROPPED_EVENTS: AtomicU64 = AtomicU64::new(0);
pub(super) static FAILED_WRITES: AtomicU64 = AtomicU64::new(0);

/// Returns snapshot metrics for the diagnostic subsystem.
#[must_use]
pub fn diagnostic_stats() -> DiagnosticStats {
    DiagnosticStats {
        dropped_events: DROPPED_EVENTS.load(Ordering::Relaxed),
        file_write_failures: FAILED_WRITES.load(Ordering::Relaxed),
    }
}
