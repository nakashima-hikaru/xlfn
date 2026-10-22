//! Runtime-owned synchronous failure history, independent of sink delivery.

use super::DiagnosticId;
use crate::execution::{CalculationId, CallId};
use crate::sync::Mutex;
use crate::{ExcelError, XllError};
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

impl DiagnosticRecord {
    fn dynamic_bytes(&self) -> usize {
        self.udf_id.capacity()
            + self.argument.as_ref().map_or(0, String::capacity)
            + self.message.capacity()
    }

    fn try_clone(&self) -> Option<Self> {
        Some(Self {
            diagnostic_id: self.diagnostic_id,
            udf_id: try_text(format_args!("{}", self.udf_id))?,
            call_id: self.call_id,
            calculation_id: self.calculation_id,
            timestamp: self.timestamp,
            error_kind: self.error_kind,
            excel_error: self.excel_error,
            argument: match &self.argument {
                Some(argument) => Some(try_text(format_args!("{argument}"))?),
                None => None,
            },
            message: try_text(format_args!("{}", self.message))?,
        })
    }
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
    record: DiagnosticRecord,
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

    fn evict_oldest(&mut self) {
        if let Some(oldest) = self.records.pop_front() {
            self.dynamic_bytes -= oldest.record.dynamic_bytes();
            self.dropped_events = self.dropped_events.saturating_add(1);
        }
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
            if page.errors.try_reserve_exact(1).is_err() {
                page.allocation_failed = true;
                return page;
            }
            let Some(record) = stored.record.try_clone() else {
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
        let mut state = self.state.lock();
        let sequence = state.next_sequence;
        state.next_sequence = state.next_sequence.saturating_add(1);
        let Some(record) = Self::make_record(metadata, error, message) else {
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
        let bytes = record.dynamic_bytes();
        if bytes > self.limits.max_bytes.saturating_sub(fixed_bytes) {
            state.dropped_events = state.dropped_events.saturating_add(1);
            return;
        }
        while state.records.len() >= self.limits.max_records
            || state.records.len() >= state.records.capacity()
            || bytes > self.limits.max_bytes.saturating_sub(state.retained_bytes())
        {
            state.evict_oldest();
        }
        state.dynamic_bytes += bytes;
        state.records.push_back(StoredRecord { sequence, record });
    }

    fn make_record(
        metadata: FailureMetadata,
        error: &XllError,
        message: Option<&str>,
    ) -> Option<DiagnosticRecord> {
        Some(DiagnosticRecord {
            diagnostic_id: metadata.diagnostic_id,
            udf_id: try_text(format_args!("{}", metadata.udf_id))?,
            call_id: metadata.call_id,
            calculation_id: metadata.calculation_id,
            timestamp: metadata.timestamp,
            error_kind: crate::execution::classify_error(error).0,
            excel_error: error.excel_error(),
            argument: match error {
                XllError::Input { argument, .. } => Some(try_text(format_args!("{argument}"))?),
                _ => None,
            },
            message: match message {
                Some(message) => try_text(format_args!("{message}"))?,
                None => try_text(format_args!("{}", super::payload::TracedError(error)))?,
            },
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
