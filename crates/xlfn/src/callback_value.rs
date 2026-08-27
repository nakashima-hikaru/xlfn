use crate::host_callback::{HostCallbackShared, HostCallbackState, observe_shared};
use crate::return_abi::{CallbackCleanupDebt, ExcelCallbackStatus};
use crate::value::{XlValueRef, XlValueType};
use crate::{XllError, XllResult};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::rc::Rc;
use xlfn_sys::{XLOPER12, excel_free, excel12_with_invocation};

const CALLBACK_CLEANUP_AUDIT_CAPACITY: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CallbackValueReleaseState {
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
/// statuses suppress read access but still allow `xlFree` cleanup once per value
/// during the active invocation lifetime. Once cleanup is indeterminate or the
/// scope is closed, Excel is no longer called.
pub(crate) struct ExcelCallbackValue {
    raw: XLOPER12,
    release_required: bool,
    audit_failures: bool,
    release_callback: ReleaseCallback,
    state: CallbackValueReleaseState,
    session: Option<Rc<HostCallbackShared>>,
    module_admission: bool,
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
            session: None,
            module_admission: false,
            _not_send_or_sync: PhantomData,
        }
    }

    #[cfg(test)]
    fn from_callback_for_test(
        raw: XLOPER12,
        status: ExcelCallbackStatus,
        release_callback: ReleaseCallback,
    ) -> Self {
        Self::from_callback_for_test_with_session(raw, status, release_callback, None)
    }

    #[cfg(test)]
    fn from_callback_for_test_with_session(
        raw: XLOPER12,
        status: ExcelCallbackStatus,
        release_callback: ReleaseCallback,
        session: Option<Rc<HostCallbackShared>>,
    ) -> Self {
        Self {
            raw,
            release_required: true,
            audit_failures: false,
            release_callback,
            state: state_after_call(true, status),
            session,
            module_admission: false,
            _not_send_or_sync: PhantomData,
        }
    }

    pub(crate) unsafe fn call_with_session(
        function: i32,
        arguments: &[NonNull<XLOPER12>],
        session: Rc<HostCallbackShared>,
    ) -> Result<(i32, Self), crate::callback_gate::CallbackAdmissionSuppressed> {
        let callback_admission = crate::callback_gate::enter_callback()?;
        // SAFETY: The caller supplies live callback arguments.
        let (status, raw, callback_invoked) =
            unsafe { excel12_with_invocation(function, arguments) };
        let callback_status = ExcelCallbackStatus::from_raw(status);
        drop(callback_admission);
        observe_shared(&session.state, callback_status);
        let state = state_after_call(callback_invoked, callback_status);
        Ok((
            status,
            Self {
                raw,
                release_required: callback_invoked,
                audit_failures: true,
                release_callback: excel_free,
                state,
                session: Some(session),
                module_admission: true,
                _not_send_or_sync: PhantomData,
            },
        ))
    }

    fn ensure_live(&self) -> XllResult<()> {
        if matches!(self.state, CallbackValueReleaseState::Live) {
            Ok(())
        } else {
            Err(XllError::input(
                "callback",
                crate::error::InputError::Malformed(
                    "callback result is unavailable after release suppression or cleanup",
                ),
            ))
        }
    }

    pub(crate) fn borrow(&mut self) -> XllResult<XlValueRef<'_>> {
        self.ensure_live()?;
        // SAFETY: `Live` means this guard still owns a readable callback result.
        unsafe { XlValueRef::from_raw(&mut self.raw) }
    }

    pub(crate) fn raw_pointer(&mut self) -> XllResult<NonNull<XLOPER12>> {
        self.ensure_live()?;
        Ok(NonNull::from_mut(&mut self.raw))
    }

    pub(crate) fn raw(&self) -> XllResult<&XLOPER12> {
        self.ensure_live()?;
        Ok(&self.raw)
    }

    pub(crate) fn value_type(&self) -> XllResult<XlValueType> {
        self.ensure_live()?;
        XlValueType::from_raw(self.raw.base_type()).ok_or_else(|| {
            XllError::input(
                "<callback>",
                crate::error::InputError::Malformed("unknown base xltype"),
            )
        })
    }

    #[must_use]
    pub(crate) const fn release_state(&self) -> CallbackValueReleaseState {
        self.state
    }

    /// Attempts cleanup once.
    ///
    /// Terminal results suppress read operations but permit `xlFree` cleanup. A
    /// failed `xlFree` is returned as an error and permanently changes the
    /// state to `Indeterminate`; neither `Drop` nor a later explicit call can
    /// retry it.
    pub(crate) fn try_release(&mut self) -> XllResult<()> {
        match self.state {
            CallbackValueReleaseState::Released => return Ok(()),
            CallbackValueReleaseState::Indeterminate { status } => {
                return Err(XllError::ExcelApi {
                    function: crate::error::ExcelApiFunction::Free,
                    failure: crate::error::ExcelApiFailure::Indeterminate(status),
                });
            }
            CallbackValueReleaseState::Live
            | CallbackValueReleaseState::TerminalSuppressed { .. } => {}
        }

        if !self.release_required {
            self.state = CallbackValueReleaseState::Released;
            return Ok(());
        }

        if let Some(session) = &self.session {
            match session.state() {
                HostCallbackState::Available | HostCallbackState::Suppressed(_) => {}
                HostCallbackState::Closed => {
                    self.state = CallbackValueReleaseState::TerminalSuppressed {
                        status: ExcelCallbackStatus::Failed(xlfn_sys::XLRET_FAILED),
                    };
                    return Ok(());
                }
            }
        }

        // Transition before invoking the host so unwinding can never leave the
        // value eligible for a second release attempt.
        self.state = CallbackValueReleaseState::Indeterminate {
            status: ExcelCallbackStatus::Failed(xlfn_sys::XLRET_FAILED),
        };

        let raw_status = if self.module_admission {
            let callback_admission = match crate::callback_gate::enter_cleanup() {
                Ok(callback_admission) => callback_admission,
                Err(suppressed) => {
                    self.state = CallbackValueReleaseState::TerminalSuppressed {
                        status: suppressed.status,
                    };
                    return Ok(());
                }
            };
            // SAFETY: `Live` / `TerminalSuppressed` plus `release_required` means this exact XLOPER12
            // was supplied as result storage to one completed callback.
            let raw_status = unsafe { (self.release_callback)(&mut self.raw) };
            drop(callback_admission);
            raw_status
        } else {
            // SAFETY: test-created values use a non-host release callback and
            // preserve the same one-release ownership invariant.
            unsafe { (self.release_callback)(&mut self.raw) }
        };
        let status = ExcelCallbackStatus::from_raw(raw_status);
        if let Some(session) = &self.session {
            observe_shared(&session.state, status);
        }

        if status == ExcelCallbackStatus::Success {
            self.state = CallbackValueReleaseState::Released;
            Ok(())
        } else {
            self.state = CallbackValueReleaseState::Indeterminate { status };
            if self.audit_failures {
                record_callback_value_debt(CallbackCleanupDebt { status });
            }
            Err(XllError::ExcelApi {
                function: crate::error::ExcelApiFunction::Free,
                failure: crate::error::ExcelApiFailure::Status(status),
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
    use crate::host_callback::HostCallbackSession;
    use std::cell::Cell;
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

    unsafe fn aborting_free(_: &mut XLOPER12) -> i32 {
        FREE_CALLS.fetch_add(1, Ordering::Relaxed);
        XLRET_ABORT
    }

    unsafe fn uncalced_free(_: &mut XLOPER12) -> i32 {
        FREE_CALLS.fetch_add(1, Ordering::Relaxed);
        XLRET_UNCALCED
    }

    #[test]
    fn terminal_statuses_call_xl_free_exactly_once() {
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
            assert_eq!(FREE_CALLS.load(Ordering::Relaxed), 1);
            drop(value);
            assert_eq!(FREE_CALLS.load(Ordering::Relaxed), 1);
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
        assert!(value.value_type().is_err());
        assert!(value.try_release().is_err());
        drop(value);
        assert_eq!(FREE_CALLS.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn xlf_register_release_abort_suppresses_unregister() {
        let _test_guard = TEST_LOCK.lock().unwrap();
        FREE_CALLS.store(0, Ordering::Relaxed);
        let session = HostCallbackSession::new();
        let mut register_result = ExcelCallbackValue::from_callback_for_test_with_session(
            XLOPER12::number(42.0),
            ExcelCallbackStatus::Success,
            aborting_free,
            Some(session.shared_handle()),
        );
        assert!(register_result.try_release().is_err());

        let followup_calls = Cell::new(0);
        assert!(
            session
                .call_for_test(|| {
                    followup_calls.set(followup_calls.get() + 1);
                    ExcelCallbackStatus::Success
                })
                .is_err()
        );
        assert_eq!(followup_calls.get(), 0);
        assert_eq!(FREE_CALLS.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn xlf_unregister_release_uncalced_suppresses_set_name() {
        let _test_guard = TEST_LOCK.lock().unwrap();
        FREE_CALLS.store(0, Ordering::Relaxed);
        let session = HostCallbackSession::new();
        let mut unregister_result = ExcelCallbackValue::from_callback_for_test_with_session(
            XLOPER12::boolean(true),
            ExcelCallbackStatus::Success,
            uncalced_free,
            Some(session.shared_handle()),
        );
        assert!(unregister_result.try_release().is_err());

        let followup_calls = Cell::new(0);
        assert!(
            session
                .call_for_test(|| {
                    followup_calls.set(followup_calls.get() + 1);
                    ExcelCallbackStatus::Success
                })
                .is_err()
        );
        assert_eq!(followup_calls.get(), 0);
        assert_eq!(FREE_CALLS.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn xlf_caller_value_drop_is_allowed_after_nested_terminal_callback() {
        let _test_guard = TEST_LOCK.lock().unwrap();
        FREE_CALLS.store(0, Ordering::Relaxed);
        let session = HostCallbackSession::new();
        let caller = ExcelCallbackValue::from_callback_for_test_with_session(
            XLOPER12::integer(1),
            ExcelCallbackStatus::Success,
            successful_free,
            Some(session.shared_handle()),
        );
        session.suppress_for_test(ExcelCallbackStatus::Abort);
        drop(caller);
        assert_eq!(FREE_CALLS.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn closed_scope_suppresses_cleanup_of_escaped_callback_values() {
        let _test_guard = TEST_LOCK.lock().unwrap();
        FREE_CALLS.store(0, Ordering::Relaxed);
        let session = HostCallbackSession::new();
        let mut value = ExcelCallbackValue::from_callback_for_test_with_session(
            XLOPER12::integer(1),
            ExcelCallbackStatus::Success,
            successful_free,
            Some(session.shared_handle()),
        );
        session.close();

        value.try_release().unwrap();
        assert!(matches!(
            value.release_state(),
            CallbackValueReleaseState::TerminalSuppressed {
                status: ExcelCallbackStatus::Failed(XLRET_FAILED)
            }
        ));
        drop(value);
        assert_eq!(FREE_CALLS.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn terminal_xl_free_allows_cleanup_of_other_live_values() {
        let _test_guard = TEST_LOCK.lock().unwrap();
        FREE_CALLS.store(0, Ordering::Relaxed);
        let session = HostCallbackSession::new();
        let mut first = ExcelCallbackValue::from_callback_for_test_with_session(
            XLOPER12::integer(1),
            ExcelCallbackStatus::Success,
            aborting_free,
            Some(session.shared_handle()),
        );
        let second = ExcelCallbackValue::from_callback_for_test_with_session(
            XLOPER12::integer(2),
            ExcelCallbackStatus::Success,
            successful_free,
            Some(session.shared_handle()),
        );
        let third = ExcelCallbackValue::from_callback_for_test_with_session(
            XLOPER12::integer(3),
            ExcelCallbackStatus::Success,
            successful_free,
            Some(session.shared_handle()),
        );

        assert!(first.try_release().is_err());
        assert_eq!(session.terminal_status(), Some(ExcelCallbackStatus::Abort));
        drop(first);
        drop(second);
        drop(third);
        assert_eq!(FREE_CALLS.load(Ordering::Relaxed), 3);
    }
}
