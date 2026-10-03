//! Stable diagnostic event and sink surface.

use crate::XllError;
use crate::diagnostics::id::DiagnosticId;
use smol_str::SmolStr;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

/// Receives detailed failures while Excel continues to receive only safe error values.
pub trait DiagnosticSink: Send + 'static {
    /// Records one event and returns in bounded time.
    ///
    /// Admitted events retain their complete structured error. The framework's
    /// asynchronous dispatcher limits queued builders to 1,024 and additional
    /// cloned payload storage, including delivery, to 16 MiB. Oversize payloads,
    /// error chains deeper than 128 nodes, and exhausted budgets are dropped
    /// before cloning and counted by [`diagnostic_stats`].
    /// Strings/paths are charged their encoded length and source Boxes their
    /// allocation size; allocator overhead and fixed channel storage are excluded.
    ///
    /// Separate opt-in tracing events retain the `error` and `error_debug` fields.
    /// Each textual representation is limited to 16 KiB plus a truncation suffix;
    /// ordinary text is unchanged. Tracing subscribers still run synchronously.
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
pub(crate) enum DiagnosticShutdownError {
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
pub struct AddinId(SmolStr);

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

        Ok(Self(SmolStr::new(value)))
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
    /// Events dropped because the queue was full or closed, the payload byte
    /// budget was exhausted, or the error exceeded the payload/depth limits.
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

#[cfg(test)]
mod tests {
    use super::AddinId;

    #[test]
    fn addin_id_keeps_owned_text_and_clones_across_storage_sizes() {
        for text in [
            "example-addin".into(),
            "a".repeat(23),
            "a".repeat(24),
            "a".repeat(64),
        ] {
            let id = AddinId::parse(&text).unwrap();
            let cloned = id.clone();
            let expected = text.clone();
            drop(text);
            assert_eq!(id.as_str(), expected);
            assert_eq!(cloned, id);
            drop(id);
            assert_eq!(cloned.as_str(), expected);
        }
    }

    #[test]
    fn addin_id_retains_basename_and_length_rules() {
        let limit = "a".repeat(64);
        assert_eq!(limit.len(), 64);
        assert_eq!(AddinId::parse(&limit).unwrap().as_str(), limit);
        assert!(AddinId::parse(&(limit + "a")).is_err());
        assert!(AddinId::parse("価格-id").is_err());
    }
}
