//! Runtime-owned synchronous failure history, independent of sink delivery.

use super::DiagnosticId;
use crate::error::DomainErrorCode;
use crate::execution::{CalculationId, CallId};
use crate::sync::Mutex;
use crate::{ExcelError, XllError};
use std::borrow::Cow;
use std::collections::VecDeque;
use std::fmt::{self, Write};
use std::time::SystemTime;

/// Classification shared with execution instrumentation.
pub use crate::execution::UdfErrorKind as DiagnosticErrorKind;

/// Position in one runtime's diagnostic history.
///
/// A cursor belongs to the history that produced it. Keep separate cursors for
/// different runtimes and start again at [`Self::START`] after loading a new DLL.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct DiagnosticCursor(u64);

impl DiagnosticCursor {
    /// Starts reading at the beginning, including any already overwritten events.
    pub const START: Self = Self(0);
}

/// An owned snapshot of a failed worksheet-function invocation.
///
/// Text fields are UTF-8 bounded to 16 KiB each. `excel_error` preserves the
/// original error, including when the FP12 ABI returns a null pointer to Excel
/// and Excel displays `#NUM!`.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct DiagnosticRecord {
    /// Correlates this failure with asynchronous sink delivery.
    pub diagnostic_id: DiagnosticId,
    /// Stable Rust identity of the worksheet function.
    pub udf_id: String,
    /// Invocation identifier, when the runtime admitted the call.
    pub call_id: Option<CallId>,
    /// Calculation generation, when it is available at the failure boundary.
    pub calculation_id: Option<CalculationId>,
    /// Wall-clock time when the failure was recorded.
    pub timestamp: SystemTime,
    /// Broad error classification.
    pub error_kind: DiagnosticErrorKind,
    /// Original worksheet error before ABI-specific conversion.
    pub excel_error: ExcelError,
    /// Named invalid argument, if present.
    pub argument: Option<String>,
    /// Bounded human-readable diagnostic detail.
    pub message: String,
}

/// A page of history, ordered by synchronous recording time.
#[derive(Debug)]
#[non_exhaustive]
pub struct DiagnosticPage {
    /// Available records following the requested cursor.
    pub errors: Vec<DiagnosticRecord>,
    /// Resume from this position on the next query.
    pub next_cursor: DiagnosticCursor,
    /// Events skipped in advancing to `next_cursor`, because they were evicted
    /// or could not be recorded. Sink queue drops are counted separately.
    pub missed_events: u64,
    /// Snapshot allocation failed. Successfully copied records and their cursor
    /// remain usable; retry from `next_cursor` to obtain the remaining records.
    pub allocation_failed: bool,
}

/// Resource bounds for one runtime's retained failure history.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiagnosticHistoryLimits {
    /// Maximum number of retained records.
    pub max_records: usize,
    /// Maximum charged bytes, including ring storage and string capacities.
    /// Allocator bookkeeping and memory owned by query snapshots are excluded.
    pub max_bytes: usize,
}

impl Default for DiagnosticHistoryLimits {
    fn default() -> Self {
        Self {
            max_records: 1024,
            max_bytes: 4 * 1024 * 1024,
        }
    }
}

/// Retention and loss counters for a runtime's history.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiagnosticHistoryStats {
    /// Current record count.
    pub retained_records: usize,
    /// Ring storage and owned string capacities, bounded by `max_bytes`.
    pub retained_bytes: usize,
    /// Records evicted or rejected since this runtime was constructed.
    pub dropped_events: u64,
}

struct StoredRecord {
    sequence: u64,
    metadata: FailureMetadata,
    error_kind: DiagnosticErrorKind,
    excel_error: ExcelError,
    argument: Option<&'static str>,
    message: StoredMessage,
}

// Keep small, self-contained failures synchronously without constructing their
// Display text on every UDF invocation. Public snapshots still own bounded text
// and use XllError's canonical Display implementation. Other failures retain
// their bounded text eagerly, so application-owned error data never escapes.
enum StoredMessage {
    Domain(DomainErrorCode),
    ExcelValue(ExcelError),
    InvalidHandle,
    StaleHandle,
    Closing,
    Overloaded,
    ReentrantCall,
    Panic,
    Internal(DiagnosticId),
    Static(&'static str),
    Text(String),
}

impl StoredMessage {
    fn new(error: &XllError, message: Option<&str>) -> Option<Self> {
        if let Some(message) = message {
            return Some(Self::Text(try_text(format_args!("{message}"))?));
        }
        Some(match error {
            XllError::Domain { code } => Self::Domain(*code),
            XllError::ExcelValue(error) => Self::ExcelValue(*error),
            XllError::InvalidHandle => Self::InvalidHandle,
            XllError::StaleHandle => Self::StaleHandle,
            XllError::Closing => Self::Closing,
            XllError::Overloaded => Self::Overloaded,
            XllError::ReentrantCall => Self::ReentrantCall,
            XllError::Panic => Self::Panic,
            XllError::Internal { diagnostic_id } => Self::Internal(*diagnostic_id),
            XllError::Custom {
                message: Cow::Borrowed(message),
                ..
            } => Self::Static(message),
            _ => Self::Text(try_text(format_args!(
                "{}",
                super::payload::TracedError(error)
            ))?),
        })
    }

    fn dynamic_bytes(&self) -> usize {
        match self {
            Self::Text(text) => text.capacity(),
            _ => 0,
        }
    }

    fn try_text(&self) -> Option<String> {
        let error = match self {
            Self::Domain(code) => XllError::Domain { code: *code },
            Self::ExcelValue(error) => XllError::ExcelValue(*error),
            Self::InvalidHandle => XllError::InvalidHandle,
            Self::StaleHandle => XllError::StaleHandle,
            Self::Closing => XllError::Closing,
            Self::Overloaded => XllError::Overloaded,
            Self::ReentrantCall => XllError::ReentrantCall,
            Self::Panic => XllError::Panic,
            Self::Internal(diagnostic_id) => XllError::Internal {
                diagnostic_id: *diagnostic_id,
            },
            Self::Static(text) => return try_text(format_args!("{text}")),
            Self::Text(text) => return try_text(format_args!("{text}")),
        };
        try_text(format_args!("{error}"))
    }
}

impl StoredRecord {
    fn try_snapshot(&self) -> Option<DiagnosticRecord> {
        Some(DiagnosticRecord {
            diagnostic_id: self.metadata.diagnostic_id,
            udf_id: try_text(format_args!("{}", self.metadata.udf_id))?,
            call_id: self.metadata.call_id,
            calculation_id: self.metadata.calculation_id,
            timestamp: self.metadata.timestamp,
            error_kind: self.error_kind,
            excel_error: self.excel_error,
            argument: match self.argument {
                Some(argument) => Some(try_text(format_args!("{argument}"))?),
                None => None,
            },
            message: self.message.try_text()?,
        })
    }
}

pub(super) struct FailureMetadata {
    pub(super) diagnostic_id: DiagnosticId,
    pub(super) udf_id: &'static str,
    pub(super) call_id: Option<CallId>,
    pub(super) calculation_id: Option<CalculationId>,
    pub(super) timestamp: SystemTime,
}

struct HistoryState {
    records: VecDeque<StoredRecord>,
    next_sequence: u64,
    dynamic_bytes: usize,
    dropped_events: u64,
}

impl HistoryState {
    const fn new() -> Self {
        Self {
            records: VecDeque::new(),
            next_sequence: 1,
            dynamic_bytes: 0,
            dropped_events: 0,
        }
    }

    fn retained_bytes(&self) -> usize {
        self.records.capacity() * size_of::<StoredRecord>() + self.dynamic_bytes
    }

    fn evict_oldest(&mut self) -> Option<StoredRecord> {
        self.records.pop_front().inspect(|oldest| {
            self.dynamic_bytes -= oldest.message.dynamic_bytes();
            self.dropped_events = self.dropped_events.saturating_add(1);
        })
    }
}

/// A bounded, synchronously populated history owned by one add-in runtime.
///
/// Queries only read history and never change UDF admission or execution state.
/// Recording is best effort: allocation failures and retention eviction are
/// visible through page gaps and [`Self::stats`]. Errors raised by Excel before
/// invoking Rust cannot be recorded here.
pub struct DiagnosticHistory {
    limits: DiagnosticHistoryLimits,
    state: Mutex<HistoryState>,
}

impl DiagnosticHistory {
    /// Constructs the default history: at most 1024 records and 4 MiB.
    #[must_use]
    pub const fn new() -> Self {
        Self::with_limits(DiagnosticHistoryLimits {
            max_records: 1024,
            max_bytes: 4 * 1024 * 1024,
        })
    }

    /// Constructs a history with explicit resource bounds.
    #[must_use]
    pub const fn with_limits(limits: DiagnosticHistoryLimits) -> Self {
        Self {
            limits,
            state: Mutex::new(HistoryState::new()),
        }
    }

    /// Returns the configured retention bounds.
    #[must_use]
    pub const fn limits(&self) -> DiagnosticHistoryLimits {
        self.limits
    }

    /// Returns at most `limit` records following `cursor`.
    ///
    /// A zero limit returns an empty page without advancing the cursor. Gaps
    /// between records and after the final record are included in
    /// `missed_events` when the returned cursor advances past them. This avoids
    /// counting the same missing event again on subsequent pages.
    #[must_use]
    pub fn errors_since(&self, cursor: DiagnosticCursor, limit: usize) -> DiagnosticPage {
        let mut page = DiagnosticPage {
            errors: Vec::new(),
            next_cursor: cursor,
            missed_events: 0,
            allocation_failed: false,
        };
        if limit == 0 {
            return page;
        }
        let state = self.state.lock();
        let latest = state.next_sequence - 1;
        for stored in state
            .records
            .iter()
            .filter(|stored| stored.sequence > cursor.0)
        {
            if page.errors.len() == limit {
                return page;
            }
            // Grow amortized while the history lock is held, preserving the
            // copied prefix and its cursor if a later reservation fails.
            if page.errors.try_reserve(1).is_err() {
                page.allocation_failed = true;
                return page;
            }
            let Some(record) = stored.try_snapshot() else {
                page.allocation_failed = true;
                return page;
            };
            page.missed_events = page.missed_events.saturating_add(
                stored
                    .sequence
                    .saturating_sub(page.next_cursor.0)
                    .saturating_sub(1),
            );
            page.next_cursor = DiagnosticCursor(stored.sequence);
            page.errors.push(record);
        }
        page.missed_events = page
            .missed_events
            .saturating_add(latest.saturating_sub(page.next_cursor.0));
        page.next_cursor = DiagnosticCursor(latest.max(page.next_cursor.0));
        page
    }

    /// Returns current retention and cumulative loss counters.
    #[must_use]
    pub fn stats(&self) -> DiagnosticHistoryStats {
        let state = self.state.lock();
        DiagnosticHistoryStats {
            retained_records: state.records.len(),
            retained_bytes: state.retained_bytes(),
            dropped_events: state.dropped_events,
        }
    }

    pub(super) fn record(
        &self,
        metadata: FailureMetadata,
        error: &XllError,
        message: Option<&str>,
    ) {
        // Formatting and fallible text allocations do not need the history
        // lock. Commit the sequence only after preparation so every visible
        // cursor still describes a completed recording attempt.
        let record = Self::make_record(metadata, error, message);
        let mut state = self.state.lock();
        let sequence = state.next_sequence;
        state.next_sequence = state.next_sequence.saturating_add(1);
        let Some(mut record) = record else {
            state.dropped_events = state.dropped_events.saturating_add(1);
            return;
        };
        // Allocate the bounded ring once. Its capacity is included in the byte
        // budget; no infallible allocation occurs in the recording path.
        if state.records.capacity() == 0 {
            let slots = self
                .limits
                .max_records
                .min(self.limits.max_bytes / size_of::<StoredRecord>());
            if slots == 0 || state.records.try_reserve_exact(slots).is_err() {
                state.dropped_events = state.dropped_events.saturating_add(1);
                return;
            }
        }
        let fixed_bytes = state.records.capacity() * size_of::<StoredRecord>();
        if fixed_bytes > self.limits.max_bytes {
            state.records = VecDeque::new();
            state.dropped_events = state.dropped_events.saturating_add(1);
            return;
        }
        let bytes = record.message.dynamic_bytes();
        if bytes > self.limits.max_bytes.saturating_sub(fixed_bytes) {
            state.dropped_events = state.dropped_events.saturating_add(1);
            return;
        }
        let mut retired = None;
        while state.records.len() >= self.limits.max_records
            || state.records.len() >= state.records.capacity()
            || bytes > self.limits.max_bytes.saturating_sub(state.retained_bytes())
        {
            retired = state.evict_oldest();
        }
        state.dynamic_bytes += bytes;
        record.sequence = sequence;
        state.records.push_back(record);
        drop(state);
        // Count-bounded steady-state recording displaces one record. Release
        // its strings after the lock; byte-pressure eviction may displace more.
        drop(retired);
    }

    fn make_record(
        metadata: FailureMetadata,
        error: &XllError,
        message: Option<&str>,
    ) -> Option<StoredRecord> {
        Some(StoredRecord {
            sequence: 0,
            metadata,
            error_kind: crate::execution::classify_error(error).0,
            excel_error: error.excel_error(),
            argument: match error {
                XllError::Input { argument, .. } => Some(argument),
                _ => None,
            },
            message: StoredMessage::new(error, message)?,
        })
    }
}

impl Default for DiagnosticHistory {
    fn default() -> Self {
        Self::new()
    }
}

struct FallibleText {
    text: String,
    failed: bool,
    truncated: bool,
}

impl Write for FallibleText {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let remaining = super::DIAGNOSTIC_TEXT_MAX_BYTES - self.text.len();
        let end = text.floor_char_boundary(text.len().min(remaining));
        if self.text.try_reserve_exact(end).is_err() {
            self.failed = true;
            return Err(fmt::Error);
        }
        self.text.push_str(&text[..end]);
        self.truncated |= end != text.len();
        Ok(())
    }
}

fn try_text(arguments: fmt::Arguments<'_>) -> Option<String> {
    let mut output = FallibleText {
        text: String::new(),
        failed: false,
        truncated: false,
    };
    if fmt::write(&mut output, arguments).is_err() || output.failed {
        return None;
    }
    if output.truncated {
        let suffix = super::DIAGNOSTIC_TRUNCATION_SUFFIX;
        let end = output
            .text
            .floor_char_boundary(super::DIAGNOSTIC_TEXT_MAX_BYTES - suffix.len());
        output.text.truncate(end);
        output.text.try_reserve_exact(suffix.len()).ok()?;
        output.text.push_str(suffix);
    }
    Some(output.text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::InputError;
    use std::sync::Arc;

    fn record(history: &DiagnosticHistory, id: u64, error: &XllError) {
        history.record(
            FailureMetadata {
                diagnostic_id: DiagnosticId::from_u64(id),
                udf_id: "prices",
                call_id: Some(CallId::new(id)),
                calculation_id: Some(CalculationId::new(7)),
                timestamp: SystemTime::UNIX_EPOCH,
            },
            error,
            None,
        );
    }

    #[test]
    fn retains_error_identity_and_original_excel_value() {
        let history = DiagnosticHistory::new();
        record(
            &history,
            41,
            &XllError::input("strike", InputError::NonFinite),
        );
        record(
            &history,
            42,
            &XllError::ExcelValue(ExcelError::NotAvailable),
        );
        let page = history.errors_since(DiagnosticCursor::START, 100);
        assert_eq!(page.errors.len(), 2);
        let input = &page.errors[0];
        assert_eq!(input.diagnostic_id.as_u64(), 41);
        assert_eq!(input.udf_id, "prices");
        assert_eq!(input.call_id.unwrap().get(), 41);
        assert_eq!(input.calculation_id.unwrap().get(), 7);
        assert_eq!(input.timestamp, SystemTime::UNIX_EPOCH);
        assert_eq!(input.error_kind, DiagnosticErrorKind::Input);
        assert_eq!(input.argument.as_deref(), Some("strike"));
        assert!(input.message.contains("strike"));
        assert_eq!(page.errors[1].error_kind, DiagnosticErrorKind::ExcelValue);
        assert_eq!(page.errors[1].excel_error, ExcelError::NotAvailable);
        assert_eq!(page.missed_events, 0);
    }

    #[test]
    fn compact_failures_keep_canonical_messages_without_owned_recording_text() {
        let history = DiagnosticHistory::new();
        let errors = [
            XllError::Domain {
                code: DomainErrorCode::InvalidInput,
            },
            XllError::Domain {
                code: DomainErrorCode::Overflow,
            },
            XllError::Domain {
                code: DomainErrorCode::NativeFailure,
            },
            XllError::ExcelValue(ExcelError::Null),
            XllError::ExcelValue(ExcelError::DivisionByZero),
            XllError::ExcelValue(ExcelError::Value),
            XllError::ExcelValue(ExcelError::Reference),
            XllError::ExcelValue(ExcelError::Name),
            XllError::ExcelValue(ExcelError::Number),
            XllError::ExcelValue(ExcelError::NotAvailable),
            XllError::ExcelValue(ExcelError::GettingData),
            XllError::InvalidHandle,
            XllError::StaleHandle,
            XllError::Closing,
            XllError::Overloaded,
            XllError::ReentrantCall,
            XllError::Panic,
            XllError::Internal {
                diagnostic_id: DiagnosticId::from_u64(0x1a2b),
            },
            XllError::custom(ExcelError::Number, "static application detail"),
        ];
        for (id, error) in errors.iter().enumerate() {
            record(&history, id as u64, error);
        }
        assert_eq!(history.state.lock().dynamic_bytes, 0);
        let page = history.errors_since(DiagnosticCursor::START, errors.len());
        assert_eq!(page.missed_events, 0);
        assert_eq!(page.errors.len(), errors.len());
        for (record, error) in page.errors.iter().zip(&errors) {
            assert_eq!(record.message, error.to_string());
            assert_eq!(record.excel_error, error.excel_error());
            assert_eq!(record.error_kind, crate::execution::classify_error(error).0);
        }
    }

    #[test]
    fn explicit_detail_overrides_compact_error_and_is_owned_synchronously() {
        let history = DiagnosticHistory::new();
        let mut detail = "panic payload with application context".to_owned();
        history.record(
            FailureMetadata {
                diagnostic_id: DiagnosticId::from_u64(1),
                udf_id: "prices",
                call_id: None,
                calculation_id: None,
                timestamp: SystemTime::UNIX_EPOCH,
            },
            &XllError::Panic,
            Some(&detail),
        );
        detail.clear();
        let page = history.errors_since(DiagnosticCursor::START, 1);
        assert_eq!(
            page.errors[0].message,
            "panic payload with application context"
        );
        assert_eq!(page.errors[0].error_kind, DiagnosticErrorKind::Panic);
        assert!(history.state.lock().dynamic_bytes > 0);
    }

    #[test]
    fn static_identity_argument_and_message_are_utf8_bounded_in_snapshots() {
        const LENGTH: usize = (super::super::DIAGNOSTIC_TEXT_MAX_BYTES / 3 + 2) * 3;
        static TEXT: [u8; LENGTH] = {
            let mut text = [0; LENGTH];
            let mut index = 0;
            while index < LENGTH {
                text[index] = 0xe6;
                text[index + 1] = 0x97;
                text[index + 2] = 0xa5;
                index += 3;
            }
            text
        };
        let text = std::str::from_utf8(&TEXT).unwrap();
        let history = DiagnosticHistory::new();
        history.record(
            FailureMetadata {
                diagnostic_id: DiagnosticId::from_u64(1),
                udf_id: text,
                call_id: None,
                calculation_id: None,
                timestamp: SystemTime::UNIX_EPOCH,
            },
            &XllError::input(text, InputError::NonFinite),
            None,
        );
        record(&history, 2, &XllError::custom(ExcelError::Value, text));
        let page = history.errors_since(DiagnosticCursor::START, 2);
        for text in [
            &page.errors[0].udf_id,
            page.errors[0].argument.as_ref().unwrap(),
            &page.errors[0].message,
            &page.errors[1].message,
        ] {
            assert!(text.len() <= super::super::DIAGNOSTIC_TEXT_MAX_BYTES);
            assert!(text.ends_with(super::super::DIAGNOSTIC_TRUNCATION_SUFFIX));
        }
        let error = XllError::custom(ExcelError::Value, text);
        assert_eq!(
            page.errors[1].message,
            try_text(format_args!(
                "{}",
                super::super::payload::TracedError(&error)
            ))
            .unwrap()
        );
    }

    #[test]
    fn compact_records_obey_fixed_byte_budget_and_expose_rejected_text_gaps() {
        let history = DiagnosticHistory::with_limits(DiagnosticHistoryLimits {
            max_records: 1,
            max_bytes: size_of::<StoredRecord>(),
        });
        record(&history, 1, &XllError::Panic);
        record(
            &history,
            2,
            &XllError::custom(ExcelError::Number, "owned detail".to_owned()),
        );
        let first = history.errors_since(DiagnosticCursor::START, 10);
        assert_eq!(first.errors.len(), 1);
        assert_eq!(first.errors[0].diagnostic_id.as_u64(), 1);
        assert_eq!(first.missed_events, 1);
        assert_eq!(history.stats().retained_bytes, size_of::<StoredRecord>());
        record(&history, 3, &XllError::Closing);
        let next = history.errors_since(first.next_cursor, 10);
        assert_eq!(next.errors.len(), 1);
        assert_eq!(next.errors[0].diagnostic_id.as_u64(), 3);
        assert_eq!(next.missed_events, 0);
        assert_eq!(history.stats().dropped_events, 2);
    }

    #[test]
    fn pages_count_overwrites_once_and_queries_do_not_consume_history() {
        let history = DiagnosticHistory::with_limits(DiagnosticHistoryLimits {
            max_records: 2,
            max_bytes: 4096,
        });
        for id in 1..=4 {
            record(&history, id, &XllError::Panic);
        }
        let first = history.errors_since(DiagnosticCursor::START, 1);
        assert_eq!(first.errors[0].diagnostic_id.as_u64(), 3);
        assert_eq!(first.missed_events, 2);
        let empty = history.errors_since(first.next_cursor, 0);
        assert_eq!(empty.next_cursor, first.next_cursor);
        assert_eq!(empty.missed_events, 0);
        let second = history.errors_since(first.next_cursor, 1);
        assert_eq!(second.errors[0].diagnostic_id.as_u64(), 4);
        assert_eq!(second.missed_events, 0);
        let end = history.errors_since(second.next_cursor, 10);
        assert!(end.errors.is_empty());
        assert_eq!(end.missed_events, 0);
        assert_eq!(history.stats().dropped_events, 2);
        assert_eq!(
            history
                .errors_since(DiagnosticCursor::START, 10)
                .errors
                .len(),
            2
        );
    }

    #[test]
    fn byte_budget_rejects_oversized_records_without_erasing_retained_records() {
        let history = DiagnosticHistory::with_limits(DiagnosticHistoryLimits {
            max_records: 2,
            max_bytes: 1024,
        });
        record(&history, 1, &XllError::Panic);
        record(
            &history,
            2,
            &XllError::custom(ExcelError::Value, "x".repeat(2048)),
        );
        record(&history, 3, &XllError::Closing);
        let page = history.errors_since(DiagnosticCursor::START, 10);
        assert_eq!(page.errors.len(), 2);
        assert_eq!(page.errors[0].diagnostic_id.as_u64(), 1);
        assert_eq!(page.errors[1].diagnostic_id.as_u64(), 3);
        assert_eq!(page.missed_events, 1);
        assert_eq!(history.stats().dropped_events, 1);
        assert!(history.stats().retained_bytes <= history.limits().max_bytes);
    }

    #[test]
    fn byte_budget_evicts_oldest_records_and_exposes_a_trailing_rejection() {
        let history = DiagnosticHistory::with_limits(DiagnosticHistoryLimits {
            max_records: 8,
            max_bytes: 8 * size_of::<StoredRecord>() + 256,
        });
        let error = XllError::custom(ExcelError::Value, "m".repeat(100));
        for id in 1..=4 {
            record(&history, id, &error);
        }
        record(
            &history,
            5,
            &XllError::custom(ExcelError::Value, "m".repeat(1024)),
        );
        let page = history.errors_since(DiagnosticCursor::START, 10);
        assert_eq!(page.errors.len(), 2);
        assert_eq!(page.errors[0].diagnostic_id.as_u64(), 3);
        assert_eq!(page.errors[1].diagnostic_id.as_u64(), 4);
        assert_eq!(page.missed_events, 3);
        let next = history.errors_since(page.next_cursor, 10);
        assert!(next.errors.is_empty());
        assert_eq!(next.missed_events, 0);
        assert_eq!(history.stats().dropped_events, 3);
        assert!(history.stats().retained_bytes <= history.limits().max_bytes);
    }

    #[test]
    fn disabled_history_exposes_trailing_recording_gaps() {
        let history = DiagnosticHistory::with_limits(DiagnosticHistoryLimits {
            max_records: 0,
            max_bytes: 0,
        });
        record(&history, 1, &XllError::Panic);
        record(&history, 2, &XllError::Panic);
        let page = history.errors_since(DiagnosticCursor::START, 10);
        assert!(page.errors.is_empty());
        assert_eq!(page.missed_events, 2);
        assert_eq!(history.errors_since(page.next_cursor, 10).missed_events, 0);
        assert_eq!(history.stats().retained_bytes, 0);
    }

    #[test]
    fn unicode_text_is_bounded_and_queries_preserve_its_truncation() {
        let history = DiagnosticHistory::new();
        record(
            &history,
            1,
            &XllError::custom(ExcelError::Number, "日本語".repeat(10000)),
        );
        let page = history.errors_since(DiagnosticCursor::START, 1);
        let message = &page.errors[0].message;
        assert!(message.len() <= super::super::DIAGNOSTIC_TEXT_MAX_BYTES);
        assert!(message.ends_with(super::super::DIAGNOSTIC_TRUNCATION_SUFFIX));
    }

    #[test]
    fn concurrent_recording_produces_one_ordered_gap_free_history() {
        let history = Arc::new(DiagnosticHistory::new());
        let workers: Vec<_> = (0..4)
            .map(|worker| {
                let history = Arc::clone(&history);
                std::thread::spawn(move || {
                    for index in 0..100 {
                        record(&history, worker * 100 + index, &XllError::Panic);
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        let mut cursor = DiagnosticCursor::START;
        let mut ids = Vec::new();
        loop {
            let page = history.errors_since(cursor, 17);
            assert_eq!(page.missed_events, 0);
            cursor = page.next_cursor;
            if page.errors.is_empty() {
                break;
            }
            ids.extend(page.errors.iter().map(|error| error.diagnostic_id.as_u64()));
        }
        ids.sort_unstable();
        assert_eq!(ids, (0..400).collect::<Vec<_>>());
        assert_eq!(history.stats().dropped_events, 0);
    }
}
