//! Allocation-free bounds for diagnostic cloning and tracing output.

use crate::XllError;
use std::borrow::Cow;
use std::fmt;

// Error chains are application-owned. Bound admission before their recursive
// derived Clone/Drop implementations can consume an unbounded worker stack.
pub(super) const MAX_ERROR_DEPTH: usize = 128;

/// Requested dynamic storage of a clone, excluding the fixed channel envelope.
/// String cloning copies length, not spare capacity. Every nested Box is charged
/// its full allocation; paths use the platform's lossless encoded byte length.
pub(super) fn clone_bytes(error: &XllError) -> Option<usize> {
    let mut current = error;
    let mut bytes = 0_usize;
    for _ in 0..MAX_ERROR_DEPTH {
        let (local, source) = match current {
            XllError::Native { message, .. } => (message.len(), None),
            XllError::Custom { message, .. } => (
                match message {
                    Cow::Borrowed(_) => 0,
                    Cow::Owned(message) => message.len(),
                },
                None,
            ),
            XllError::LibraryLoad { path, .. } => (path.as_os_str().as_encoded_bytes().len(), None),
            XllError::RtdSubscriptionShutdown { key, source, .. } => (
                key.len().checked_add(size_of::<XllError>())?,
                Some(&**source),
            ),
            XllError::RtdProducerFailure { topic, source } => (
                topic.len().checked_add(size_of::<XllError>())?,
                Some(&**source),
            ),
            XllError::Input { .. }
            | XllError::Shape { .. }
            | XllError::ElementCountMismatch { .. }
            | XllError::Domain { .. }
            | XllError::ExcelApi { .. }
            | XllError::WindowsApi { .. }
            | XllError::RegistrationConflict { .. }
            | XllError::MetadataDebtBindingChanged { .. }
            | XllError::MissingSymbol { .. }
            | XllError::AbiMismatch { .. }
            | XllError::ExcelValue(_)
            | XllError::InvalidHandle
            | XllError::StaleHandle
            | XllError::Closing
            | XllError::Overloaded
            | XllError::ReentrantCall
            | XllError::Panic
            | XllError::Internal { .. } => (0, None),
        };
        bytes = bytes.checked_add(local)?;
        if bytes > super::DIAGNOSTIC_PAYLOAD_MAX_BYTES {
            return None;
        }
        let Some(source) = source else {
            return Some(bytes);
        };
        current = source;
    }
    None
}

/// Keeps tracing's two existing fields and ordinary Display/Debug text intact.
/// Each representation emits at most the text limit plus a truncation suffix;
/// the admitted DiagnosticSink still receives the original structured error.
pub(super) struct TracedError<'a>(pub(super) &'a XllError);

struct LimitedWriter<'a, 'b> {
    output: &'a mut fmt::Formatter<'b>,
    remaining: usize,
    truncated: bool,
}

impl fmt::Write for LimitedWriter<'_, '_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        if text.len() <= self.remaining {
            self.output.write_str(text)?;
            self.remaining -= text.len();
            return Ok(());
        }
        let end = text.floor_char_boundary(self.remaining);
        self.output.write_str(&text[..end])?;
        self.remaining = 0;
        self.truncated = true;
        Err(fmt::Error)
    }
}

fn trace_format(output: &mut fmt::Formatter<'_>, arguments: fmt::Arguments<'_>) -> fmt::Result {
    let mut limited = LimitedWriter {
        output,
        remaining: super::DIAGNOSTIC_TEXT_MAX_BYTES,
        truncated: false,
    };
    let result = fmt::write(&mut limited, arguments);
    if limited.truncated {
        limited
            .output
            .write_str(super::DIAGNOSTIC_TRUNCATION_SUFFIX)
    } else {
        result
    }
}

impl fmt::Display for TracedError<'_> {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        trace_format(output, format_args!("{}", self.0))
    }
}

impl fmt::Debug for TracedError<'_> {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        trace_format(output, format_args!("{:?}", DebugError(self.0)))
    }
}

// Limit string input before Debug performs escape scanning. Limiting only the
// writer would still scan an entire giant String to find its escaped chunks.
struct DebugText<'a>(&'a str);

impl fmt::Debug for DebugText<'_> {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        let end = self
            .0
            .floor_char_boundary(self.0.len().min(super::DIAGNOSTIC_TEXT_MAX_BYTES));
        fmt::Debug::fmt(&self.0[..end], output)
    }
}

struct DebugError<'a>(&'a XllError);

impl fmt::Debug for DebugError<'_> {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            XllError::Custom { excel, message } => output
                .debug_struct("Custom")
                .field("excel", excel)
                .field("message", &DebugText(message))
                .finish(),
            XllError::Native { code, message } => output
                .debug_struct("Native")
                .field("code", code)
                .field("message", &DebugText(message))
                .finish(),
            XllError::RtdProducerFailure { topic, source } => output
                .debug_struct("RtdProducerFailure")
                .field("topic", &DebugText(topic))
                .field("source", &DebugError(source))
                .finish(),
            XllError::RtdSubscriptionShutdown {
                server_generation,
                topic_id,
                key,
                source,
            } => output
                .debug_struct("RtdSubscriptionShutdown")
                .field("server_generation", server_generation)
                .field("topic_id", topic_id)
                .field("key", &DebugText(key))
                .field("source", &DebugError(source))
                .finish(),
            other => fmt::Debug::fmt(other, output),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_errors_charge_only_owned_messages_and_bound_tracing_text() {
        use crate::error::ExcelError;
        let borrowed = XllError::custom(ExcelError::NotAvailable, "static application detail");
        let owned = XllError::custom(ExcelError::NotAvailable, "owned detail".to_owned());
        assert_eq!(clone_bytes(&borrowed), Some(0));
        assert_eq!(clone_bytes(&owned), Some(12));
        assert_eq!(format!("{}", TracedError(&owned)), owned.to_string());
        assert_eq!(format!("{:?}", TracedError(&owned)), format!("{owned:?}"));

        let oversize = XllError::custom(
            ExcelError::NotAvailable,
            "x".repeat(super::super::DIAGNOSTIC_PAYLOAD_MAX_BYTES + 1),
        );
        assert_eq!(clone_bytes(&oversize), None);
        for text in [
            format!("{}", TracedError(&oversize)),
            format!("{:?}", TracedError(&oversize)),
        ] {
            assert!(text.ends_with(super::super::DIAGNOSTIC_TRUNCATION_SUFFIX));
            assert!(
                text.len()
                    <= super::super::DIAGNOSTIC_TEXT_MAX_BYTES
                        + super::super::DIAGNOSTIC_TRUNCATION_SUFFIX.len()
            );
        }
    }

    #[test]
    fn clone_budget_counts_owned_lengths_boxes_and_paths() {
        let mut message = String::with_capacity(4096);
        message.push_str("abc");
        let error = XllError::RtdProducerFailure {
            topic: "topic".to_owned(),
            source: Box::new(XllError::RtdSubscriptionShutdown {
                server_generation: 1,
                topic_id: 2,
                key: "key".to_owned(),
                source: Box::new(XllError::Native { code: 1, message }),
            }),
        };
        assert_eq!(clone_bytes(&error), Some(11 + 2 * size_of::<XllError>()));
        let path = std::path::PathBuf::from("native/日本語.dll");
        let bytes = path.as_os_str().as_encoded_bytes().len();
        assert_eq!(
            clone_bytes(&XllError::LibraryLoad { path, os_error: 1 }),
            Some(bytes)
        );
        assert_eq!(clone_bytes(&XllError::Panic), Some(0));
    }

    #[test]
    fn deep_structured_errors_are_rejected_before_recursive_clone() {
        let mut error = XllError::Panic;
        for _ in 1..MAX_ERROR_DEPTH {
            error = XllError::RtdProducerFailure {
                topic: String::new(),
                source: Box::new(error),
            };
        }
        assert_eq!(
            clone_bytes(&error),
            Some((MAX_ERROR_DEPTH - 1) * size_of::<XllError>())
        );
        error = XllError::RtdProducerFailure {
            topic: String::new(),
            source: Box::new(error),
        };
        assert_eq!(clone_bytes(&error), None);
    }

    #[test]
    fn tracing_preserves_ordinary_text_and_bounds_both_large_representations() {
        let small = XllError::Native {
            code: 7,
            message: "日本語 quoted \"text\"".to_owned(),
        };
        assert_eq!(format!("{}", TracedError(&small)), small.to_string());
        assert_eq!(format!("{:?}", TracedError(&small)), format!("{small:?}"));
        let large = XllError::Native {
            code: 1,
            message: "語".repeat(super::super::DIAGNOSTIC_TEXT_MAX_BYTES),
        };
        for text in [
            format!("{}", TracedError(&large)),
            format!("{:?}", TracedError(&large)),
        ] {
            assert!(text.ends_with(super::super::DIAGNOSTIC_TRUNCATION_SUFFIX));
            assert!(
                text.len()
                    <= super::super::DIAGNOSTIC_TEXT_MAX_BYTES
                        + super::super::DIAGNOSTIC_TRUNCATION_SUFFIX.len()
            );
        }
    }
}
