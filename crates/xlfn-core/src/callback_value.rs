use crate::{CallbackCleanupDebt, ExcelCallbackStatus, ExcelValueRef, XllError, XllResult};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::rc::Rc;
use xlfn_sys::{XLOPER12, excel_free, excel12_with_invocation};

const CALLBACK_CLEANUP_AUDIT_CAPACITY: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallbackValueReleaseState {
    Live,
    Released,
    TerminalSuppressed { status: ExcelCallbackStatus },
    Indeterminate { status: ExcelCallbackStatus },
}

#[derive(Debug, Default)]
struct CallbackCleanupAudit {
    debts: VecDeque<CallbackCleanupDebt>,
    dropped_records: usize,
}

static CALLBACK_CLEANUP_AUDIT: Mutex<CallbackCleanupAudit> = Mutex::new(CallbackCleanupAudit {
    debts: VecDeque::new(),
    dropped_records: 0,
});

fn record_callback_value_debt(debt: CallbackCleanupDebt) {
    let mut audit = CALLBACK_CLEANUP_AUDIT.lock();
    if audit.debts.len() == CALLBACK_CLEANUP_AUDIT_CAPACITY {
        audit.debts.pop_front();
        audit.dropped_records = audit.dropped_records.saturating_add(1);
    }
    audit.debts.push_back(debt);
}

pub(crate) fn callback_cleanup_debt_is_empty() -> bool {
    let audit = CALLBACK_CLEANUP_AUDIT.lock();
    audit.debts.is_empty() && audit.dropped_records == 0
}

type ReleaseCallback = unsafe fn(&mut XLOPER12) -> i32;

fn state_after_call(
    callback_invoked: bool,
    status: ExcelCallbackStatus,
) -> CallbackValueReleaseState {
    if !callback_invoked {
        CallbackValueReleaseState::Released
    } else if status.is_terminal() {
        CallbackValueReleaseState::TerminalSuppressed { status }
    } else {
        CallbackValueReleaseState::Live
    }
}

/// Owns one result returned by an Excel callback for the rest of its call scope.
///
/// A live Excel-owned value gets at most one `xlFree` attempt. Terminal callback
/// statuses suppress cleanup completely because Excel forbids further C API
/// calls after `xlretAbort` and `xlretUncalced`. Once cleanup is indeterminate,
/// the value can neither be read nor released again.
pub struct ExcelCallbackValue {
    raw: XLOPER12,
    release_required: bool,
    audit_failures: bool,
    release_callback: ReleaseCallback,
    state: CallbackValueReleaseState,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl ExcelCallbackValue {
    #[cfg(test)]
    pub(crate) const fn from_raw_for_test(raw: XLOPER12) -> Self {
        Self {
            raw,
            release_required: false,
            audit_failures: false,
            release_callback: excel_free,
            state: CallbackValueReleaseState::Live,
            _not_send_or_sync: PhantomData,
        }
    }

    #[cfg(test)]
    fn from_callback_for_test(
        raw: XLOPER12,
        status: ExcelCallbackStatus,
        release_callback: ReleaseCallback,
    ) -> Self {
        Self {
            raw,
            release_required: true,
            audit_failures: false,
            release_callback,
            state: state_after_call(true, status),
            _not_send_or_sync: PhantomData,
        }
    }

    pub(crate) unsafe fn call(function: i32, arguments: &[NonNull<XLOPER12>]) -> (i32, Self) {
        // SAFETY: The caller supplies live callback arguments.
        let (status, raw, callback_invoked) =
            unsafe { excel12_with_invocation(function, arguments) };
        let callback_status = ExcelCallbackStatus::from_raw(status);
        let state = state_after_call(callback_invoked, callback_status);
        (
            status,
            Self {
                raw,
                release_required: callback_invoked,
                audit_failures: true,
                release_callback: excel_free,
                state,
                _not_send_or_sync: PhantomData,
            },
        )
    }

    fn ensure_live(&self) -> XllResult<()> {
        if matches!(self.state, CallbackValueReleaseState::Live) {
            Ok(())
        } else {
            Err(XllError::input(
                "callback",
                crate::InputError::Malformed(
                    "callback result is unavailable after release suppression or cleanup",
                ),
            ))
        }
    }

    pub fn borrow(&mut self) -> XllResult<ExcelValueRef<'_>> {
        self.ensure_live()?;
        // SAFETY: `Live` means this guard still owns a readable callback result.
        unsafe { ExcelValueRef::from_raw(&mut self.raw) }
    }

    pub(crate) fn raw_pointer(&mut self) -> XllResult<NonNull<XLOPER12>> {
        self.ensure_live()?;
        Ok(NonNull::from(&mut self.raw))
    }

    pub(crate) fn raw(&self) -> XllResult<&XLOPER12> {
        self.ensure_live()?;
        Ok(&self.raw)
    }

    pub fn base_type(&self) -> XllResult<u32> {
        self.ensure_live()?;
        Ok(self.raw.base_type())
    }

    #[must_use]
    pub const fn release_state(&self) -> CallbackValueReleaseState {
        self.state
    }

    /// Attempts cleanup once.
    ///
    /// Terminal results are intentionally treated as already suppressed. A
    /// failed `xlFree` is returned as an error and permanently changes the
    /// state to `Indeterminate`; neither `Drop` nor a later explicit call can
    /// retry it.
    pub fn try_release(&mut self) -> XllResult<()> {
        match self.state {
            CallbackValueReleaseState::Released
            | CallbackValueReleaseState::TerminalSuppressed { .. } => return Ok(()),
            CallbackValueReleaseState::Indeterminate { status } => {
                return Err(XllError::ExcelApi {
                    function: "xlFree(indeterminate callback result)",
                    code: status.raw_code(),
                });
            }
            CallbackValueReleaseState::Live => {}
        }

        if !self.release_required {
            self.state = CallbackValueReleaseState::Released;
            return Ok(());
        }

        // Transition before invoking the host so unwinding can never leave the
        // value eligible for a second release attempt.
        self.state = CallbackValueReleaseState::Indeterminate {
            status: ExcelCallbackStatus::Failed(xlfn_sys::XLRET_FAILED),
        };
        // SAFETY: `Live` plus `release_required` means this exact XLOPER12 was
        // supplied as result storage to one completed, non-terminal callback.
        let raw_status = unsafe { (self.release_callback)(&mut self.raw) };
        let status = ExcelCallbackStatus::from_raw(raw_status);

        if status == ExcelCallbackStatus::Success {
            self.state = CallbackValueReleaseState::Released;
            Ok(())
        } else {
            self.state = CallbackValueReleaseState::Indeterminate { status };
            if self.audit_failures {
                record_callback_value_debt(CallbackCleanupDebt { status });
            }
            Err(XllError::ExcelApi {
                function: "xlFree",
                code: raw_status,
            })
        }
    }
}

impl Drop for ExcelCallbackValue {
    fn drop(&mut self) {
        if let Err(error) = self.try_release() {
            crate::diagnostics::report_no_unwind("Excel callback result cleanup", &error);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use xlfn_sys::{XLRET_ABORT, XLRET_FAILED, XLRET_SUCCESS, XLRET_UNCALCED};

    static FREE_CALLS: AtomicUsize = AtomicUsize::new(0);
    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    unsafe fn successful_free(_: &mut XLOPER12) -> i32 {
        FREE_CALLS.fetch_add(1, Ordering::Relaxed);
        XLRET_SUCCESS
    }

    unsafe fn failed_free(_: &mut XLOPER12) -> i32 {
        FREE_CALLS.fetch_add(1, Ordering::Relaxed);
        XLRET_FAILED
    }

    #[test]
    fn terminal_statuses_never_call_xl_free() {
        let _test_guard = TEST_LOCK.lock().unwrap();
        for status in [XLRET_ABORT, XLRET_UNCALCED] {
            FREE_CALLS.store(0, Ordering::Relaxed);
            let mut value = ExcelCallbackValue::from_callback_for_test(
                XLOPER12::integer(1),
                ExcelCallbackStatus::from_raw(status),
                successful_free,
            );
            assert!(matches!(
                value.release_state(),
                CallbackValueReleaseState::TerminalSuppressed { .. }
            ));
            assert!(value.borrow().is_err());
            assert!(value.raw().is_err());
            assert!(value.raw_pointer().is_err());
            value.try_release().unwrap();
            drop(value);
            assert_eq!(FREE_CALLS.load(Ordering::Relaxed), 0);
        }
    }

    #[test]
    fn indeterminate_cleanup_is_attempted_once_and_cannot_be_borrowed() {
        let _test_guard = TEST_LOCK.lock().unwrap();
        FREE_CALLS.store(0, Ordering::Relaxed);
        let mut value = ExcelCallbackValue::from_callback_for_test(
            XLOPER12::integer(1),
            ExcelCallbackStatus::Success,
            failed_free,
        );

        assert!(value.try_release().is_err());
        assert_eq!(FREE_CALLS.load(Ordering::Relaxed), 1);
        assert!(matches!(
            value.release_state(),
            CallbackValueReleaseState::Indeterminate {
                status: ExcelCallbackStatus::Failed(XLRET_FAILED)
            }
        ));
        assert!(value.borrow().is_err());
        assert!(value.raw().is_err());
        assert!(value.raw_pointer().is_err());
        assert!(value.base_type().is_err());
        assert!(value.try_release().is_err());
        drop(value);
        assert_eq!(FREE_CALLS.load(Ordering::Relaxed), 1);
    }
}
