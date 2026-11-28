//! Allocation-free bounds for diagnostic cloning and tracing output.

use crate::{XllError, error::InputError};
use std::fmt;

// Error chains are application-owned. Bound admission before their recursive
// derived Clone/Drop implementations can consume an unbounded worker stack.
pub(super) const MAX_ERROR_DEPTH: usize = 128;

pub(super) fn clone_bytes(error: &XllError) -> Option<usize> {
    crate::error::construct::clone_bytes(
        error,
        super::DIAGNOSTIC_PAYLOAD_MAX_BYTES,
        MAX_ERROR_DEPTH,
    )
}

/// Keeps tracing's two existing fields and ordinary Display/Debug text intact.
/// Each representation emits at most the text limit plus a truncation suffix;
/// the admitted DiagnosticSink still receives the original structured error.
pub(crate) struct TracedError<'a>(pub(super) &'a XllError);

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
        trace_format(output, format_args!("{}", DisplayError(self.0)))
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

struct DebugUnknownEnum<'a> {
    actual: &'a str,
    expected: &'a str,
}

impl fmt::Debug for DebugUnknownEnum<'_> {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("UnknownEnum")
            .field("actual_bytes", &self.actual.len())
            .field("expected", &DebugText(self.expected))
            .finish()
    }
}

struct DisplayError<'a>(&'a XllError);

impl fmt::Display for DisplayError<'_> {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        use crate::error::construct::{ErrorView, view};
        match view(self.0) {
            ErrorView::Application {
                message, source, ..
            } => {
                output.write_str(message)?;
                let mut cause = source.map(|source| source as &dyn std::error::Error);
                while let Some(current) = cause {
                    write!(output, ": {current}")?;
                    cause = current.source();
                }
                Ok(())
            }

            ErrorView::Input {
                argument,
                reason: InputError::UnknownEnum { actual, expected },
            } => {
                write!(
                    output,
                    "invalid argument {argument}: expected one of {expected}, received {} bytes (redacted)",
                    actual.len()
                )
            }
            ErrorView::RtdProducerFailure { topic, source } => {
                write!(
                    output,
                    "RTD producer failed for topic ({} bytes, redacted): {}",
                    topic.len(),
                    DisplayError(source)
                )
            }
            ErrorView::RtdSubscriptionShutdown {
                server_generation,
                topic_id,
                key,
                source,
            } => {
                write!(
                    output,
                    "RTD subscription shutdown failed for server generation {server_generation}, topic {topic_id}, key {key}: {}",
                    DisplayError(source)
                )
            }
            _ => fmt::Display::fmt(self.0, output),
        }
    }
}

struct DebugError<'a>(&'a XllError);

impl fmt::Debug for DebugError<'_> {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        use crate::error::construct::{ErrorView, view};
        match view(self.0) {
            ErrorView::Input {
                argument,
                reason: InputError::UnknownEnum { actual, expected },
            } => output
                .debug_struct("Input")
                .field("argument", &DebugText(argument))
                .field("reason", &DebugUnknownEnum { actual, expected })
                .finish(),
            ErrorView::Custom { excel, message } => output
                .debug_struct("Custom")
                .field("excel", &excel)
                .field("message", &DebugText(message))
                .finish(),
            ErrorView::Native { code, message } => output
                .debug_struct("Native")
                .field("code", &code)
                .field("message", &DebugText(message))
                .finish(),
            ErrorView::RtdProducerFailure { topic, source } => output
                .debug_struct("RtdProducerFailure")
                .field("topic_bytes", &topic.len())
                .field("source", &DebugError(source))
                .finish(),
            ErrorView::RtdSubscriptionShutdown {
                server_generation,
                topic_id,
                key,
                source,
            } => output
                .debug_struct("RtdSubscriptionShutdown")
                .field("server_generation", &server_generation)
                .field("topic_id", &topic_id)
                .field("key", &DebugText(key))
                .field("source", &DebugError(source))
                .finish(),
            _ => fmt::Debug::fmt(self.0, output),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_errors_charge_only_owned_messages_and_bound_tracing_text() {
        use crate::error::ExcelError;
        let boxed_size = crate::error::construct::boxed_error_size();
        let borrowed = XllError::custom(ExcelError::NotAvailable, "static application detail");
        let owned = XllError::custom(ExcelError::NotAvailable, "owned detail".to_owned());
        assert_eq!(clone_bytes(&borrowed), Some(boxed_size));
        assert_eq!(clone_bytes(&owned), Some(boxed_size + 12));
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
    fn enum_errors_charge_owned_input_and_bound_escaped_tracing() {
        let boxed_size = crate::error::construct::boxed_error_size();
        let small = XllError::input(
            "mode",
            InputError::UnknownEnum {
                actual: "unknown\n\"value".to_owned(),
                expected: "[\"Fast\", \"Slow\"]",
            },
        );
        assert_eq!(
            clone_bytes(&small),
            Some(boxed_size + "unknown\n\"value".len())
        );
        for text in [
            format!("{}", TracedError(&small)),
            format!("{:?}", TracedError(&small)),
        ] {
            assert!(!text.contains("unknown"));
            assert!(!text.contains("value"));
        }
        let large = XllError::input(
            "mode",
            InputError::UnknownEnum {
                actual: "\n".repeat(super::super::DIAGNOSTIC_PAYLOAD_MAX_BYTES + 1),
                expected: "[Fast]",
            },
        );
        assert_eq!(clone_bytes(&large), None);
        for error in [
            &large,
            &crate::error::construct::rtd_producer_failure("test".to_owned(), large.clone()),
        ] {
            for text in [
                format!("{}", TracedError(error)),
                format!("{:?}", TracedError(error)),
            ] {
                assert!(!text.contains("\\n"));
                assert!(
                    text.len()
                        <= super::super::DIAGNOSTIC_TEXT_MAX_BYTES
                            + super::super::DIAGNOSTIC_TRUNCATION_SUFFIX.len()
                );
            }
        }
    }

    #[test]
    fn clone_budget_counts_owned_lengths_boxes_and_paths() {
        let boxed_size = crate::error::construct::boxed_error_size();
        let mut message = String::with_capacity(4096);
        message.push_str("abc");
        let error = crate::error::construct::rtd_producer_failure(
            "topic".to_owned(),
            crate::error::construct::rtd_subscription_shutdown(
                1,
                2,
                "key".to_owned(),
                XllError::native(1, message),
            ),
        );
        assert_eq!(clone_bytes(&error), Some(11 + 3 * boxed_size));
        let path = std::path::PathBuf::from("native/日本語.dll");
        let bytes = path.as_os_str().as_encoded_bytes().len();
        assert_eq!(
            clone_bytes(&crate::error::construct::library_load(path, 1)),
            Some(boxed_size + bytes)
        );
        assert_eq!(clone_bytes(&XllError::PANIC), Some(0));
    }

    #[test]
    fn deep_structured_errors_are_rejected_before_recursive_clone() {
        let boxed_size = crate::error::construct::boxed_error_size();
        let mut error = XllError::PANIC;
        for _ in 1..MAX_ERROR_DEPTH {
            error = crate::error::construct::rtd_producer_failure(String::new(), error);
        }
        assert_eq!(
            clone_bytes(&error),
            Some((MAX_ERROR_DEPTH - 1) * boxed_size)
        );
        error = crate::error::construct::rtd_producer_failure(String::new(), error);
        assert_eq!(clone_bytes(&error), None);
    }

    #[test]
    fn tracing_preserves_ordinary_text_and_bounds_both_large_representations() {
        let small = XllError::native(7, "日本語 quoted \"text\"");
        assert_eq!(format!("{}", TracedError(&small)), small.to_string());
        assert_eq!(format!("{:?}", TracedError(&small)), format!("{small:?}"));
        let large = XllError::native(1, "語".repeat(super::super::DIAGNOSTIC_TEXT_MAX_BYTES));
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
