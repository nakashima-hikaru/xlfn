use crate::host_callback::HostCallbackSession;
use crate::return_abi::{CallbackCleanupDebt, ExcelCallbackStatus};
use crate::sync::Mutex;
use crate::value::{XlValueRef, XlValueType};
use crate::{XllError, XllResult};
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

#[cfg(test)]
type ReleaseCallback = unsafe fn(&mut XLOPER12) -> i32;

#[cfg(test)]
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
/// during the active invocation lifetime. Once cleanup is indeterminate,
/// Excel is no longer called.
struct CallbackOwner<'session> {
    raw: XLOPER12,
    release_required: bool,
    #[cfg(test)]
    audit_failures: bool,
    #[cfg(test)]
    release_callback: ReleaseCallback,
    state: CallbackValueReleaseState,
    session: Option<&'session HostCallbackSession>,
    module_permit: Option<crate::callback_gate::ModuleCallbackPermit>,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

/// Read access exists only while the underlying owner has not begun release.
pub(crate) struct ExcelCallbackValue<'session> {
    owner: CallbackOwner<'session>,
}

/// Dynamic callback evidence is classified once; terminal results carry
/// cleanup authority without exposing a readable owner.
pub(crate) enum CallbackResult<'session> {
    Readable(ExcelCallbackValue<'session>),
    CleanupOnly(CleanupOnlyValue<'session>),
    Empty,
}
pub(crate) struct CleanupOnlyValue<'session> {
    owner: CallbackOwner<'session>,
}
impl<'session> CallbackResult<'session> {
    #[cfg(test)]
    fn callback_for_test(
        raw: XLOPER12,
        status: ExcelCallbackStatus,
        release: ReleaseCallback,
    ) -> Self {
        let owner = CallbackOwner::from_callback_for_test(raw, status, release);
        if status.is_terminal() {
            Self::CleanupOnly(CleanupOnlyValue { owner })
        } else {
            Self::Readable(ExcelCallbackValue { owner })
        }
    }
    pub(crate) fn readable(&mut self) -> XllResult<&mut ExcelCallbackValue<'session>> {
        match self {
            Self::Readable(owner) => Ok(owner),
            _ => Err(XllError::input(
                "callback",
                crate::error::InputError::Malformed("callback result is not readable"),
            )),
        }
    }
    pub(crate) fn release(self) -> XllResult<()> {
        match self {
            Self::Readable(owner) => owner.release(),
            Self::CleanupOnly(owner) => owner.release(),
            Self::Empty => Ok(()),
        }
    }
}
impl CleanupOnlyValue<'_> {
    pub(crate) fn release(self) -> XllResult<()> {
        self.owner.release()
    }
}

impl CallbackOwner<'static> {
    #[cfg(test)]
    pub(crate) const fn from_raw_for_test(raw: XLOPER12) -> Self {
        Self {
            raw,
            release_required: false,
            audit_failures: false,
            release_callback: excel_free,
            state: CallbackValueReleaseState::Live,
            session: None,
            module_permit: None,
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
            session: None,
            module_permit: None,
            _not_send_or_sync: PhantomData,
        }
    }
}

impl<'session> CallbackOwner<'session> {
    #[cfg(test)]
    fn from_callback_for_test_with_session(
        raw: XLOPER12,
        status: ExcelCallbackStatus,
        release_callback: ReleaseCallback,
        session: &'session HostCallbackSession,
    ) -> Self {
        Self {
            raw,
            release_required: true,
            audit_failures: false,
            release_callback,
            state: state_after_call(true, status),
            session: Some(session),
            module_permit: None,
            _not_send_or_sync: PhantomData,
        }
    }

    #[cfg(test)]
    fn from_callback_for_test_with_permit(
        raw: XLOPER12,
        status: ExcelCallbackStatus,
        release_callback: ReleaseCallback,
        session: &'session HostCallbackSession,
        module_permit: Option<crate::callback_gate::ModuleCallbackPermit>,
    ) -> Self {
        Self {
            raw,
            release_required: true,
            audit_failures: false,
            release_callback,
            state: state_after_call(true, status),
            session: Some(session),
            module_permit,
            _not_send_or_sync: PhantomData,
        }
    }
}

impl<'session> ExcelCallbackValue<'session> {
    #[cfg(test)]
    pub(crate) const fn from_raw_for_test(raw: XLOPER12) -> Self {
        Self {
            owner: CallbackOwner::from_raw_for_test(raw),
        }
    }

    pub(crate) unsafe fn call_with_session(
        function: i32,
        arguments: &[NonNull<XLOPER12>],
        session: &'session HostCallbackSession,
    ) -> Result<(i32, CallbackResult<'session>), crate::callback_gate::CallbackAdmissionSuppressed>
    {
        let callback_admission = crate::callback_gate::enter_callback()?;
        // SAFETY: The caller supplies live callback arguments.
        let (status, raw, callback_invoked) =
            unsafe { excel12_with_invocation(function, arguments) };
        let callback_status = ExcelCallbackStatus::from_raw(status);
        if !callback_invoked {
            drop(callback_admission);
            return Ok((status, CallbackResult::Empty));
        }
        // Classification constructs the fresh owner itself. A value whose
        // cleanup already started cannot be supplied to this issuing path.
        let make_owner = |state| CallbackOwner {
            raw,
            release_required: true,
            #[cfg(test)]
            audit_failures: true,
            #[cfg(test)]
            release_callback: excel_free,
            state,
            session: Some(session),
            module_permit: Some(callback_admission),
            _not_send_or_sync: PhantomData,
        };
        let result = if callback_status.is_terminal() {
            CallbackResult::CleanupOnly(CleanupOnlyValue {
                owner: make_owner(CallbackValueReleaseState::TerminalSuppressed {
                    status: callback_status,
                }),
            })
        } else {
            CallbackResult::Readable(ExcelCallbackValue {
                owner: make_owner(CallbackValueReleaseState::Live),
            })
        };
        Ok((status, result))
    }

    pub(crate) fn borrow(&mut self) -> XllResult<XlValueRef<'_>> {
        // SAFETY: only a fresh, readable callback result constructs this wrapper.
        // Releasing consumes it, and returned borrows cannot outlive that move.
        unsafe { XlValueRef::from_raw(&mut self.owner.raw) }
    }

    /// The returned raw pointer is valid only while this owner remains live and
    /// stationary, including the whole synchronous callback that consumes it.
    /// The pointer itself carries no lifetime and must not be retained.
    #[cfg(feature = "handles")]
    pub(crate) fn raw_pointer(&mut self) -> NonNull<XLOPER12> {
        NonNull::from_mut(&mut self.owner.raw)
    }

    pub(crate) fn raw(&self) -> &XLOPER12 {
        &self.owner.raw
    }

    pub(crate) fn value_type(&self) -> XllResult<XlValueType> {
        XlValueType::from_raw(self.owner.raw.base_type()).ok_or_else(|| {
            XllError::input(
                "<callback>",
                crate::error::InputError::Malformed("unknown base xltype"),
            )
        })
    }

    /// Consumes readable authority even if cleanup fails.
    pub(crate) fn release(self) -> XllResult<()> {
        self.owner.release()
    }
}

impl CallbackOwner<'_> {
    fn release(mut self) -> XllResult<()> {
        self.release_in_place()
    }

    /// Attempts cleanup once.
    ///
    /// Terminal results suppress read operations but permit `xlFree` cleanup. A
    /// failed `xlFree` is returned as an error and permanently changes the
    /// state to `Indeterminate`; neither `Drop` nor a later explicit call can
    /// retry it.
    fn release_in_place(&mut self) -> XllResult<()> {
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
            drop(self.module_permit.take());
            self.state = CallbackValueReleaseState::Released;
            return Ok(());
        }

        // Transition before invoking the host so unwinding can never leave the
        // value eligible for a second release attempt.
        self.state = CallbackValueReleaseState::Indeterminate {
            status: ExcelCallbackStatus::Failed(xlfn_sys::XLRET_FAILED),
        };

        let permit = self.module_permit.take();
        #[cfg(test)]
        let release = self.release_callback;
        #[cfg(not(test))]
        let release = excel_free;
        // SAFETY: `Live` / `TerminalSuppressed` plus `release_required` means this exact XLOPER12
        // was supplied as result storage to one completed callback.
        let raw_status = unsafe { release(&mut self.raw) };
        drop(permit);

        let status = ExcelCallbackStatus::from_raw(raw_status);
        if let Some(session) = self.session {
            session.observe(status);
        }

        if status == ExcelCallbackStatus::Success {
            self.state = CallbackValueReleaseState::Released;
            Ok(())
        } else {
            self.state = CallbackValueReleaseState::Indeterminate { status };
            #[cfg(test)]
            let audit_failures = self.audit_failures;
            #[cfg(not(test))]
            let audit_failures = true;
            if audit_failures {
                record_callback_value_debt(CallbackCleanupDebt { status });
            }
            Err(XllError::ExcelApi {
                function: crate::error::ExcelApiFunction::Free,
                failure: crate::error::ExcelApiFailure::Status(status),
            })
        }
    }
}

impl Drop for CallbackOwner<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.release_in_place() {
            crate::diagnostics::report_no_unwind("Excel callback result cleanup", &error);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn miri_callback_result_classifies_read_and_cleanup_authority() {
        let _test_guard = TEST_LOCK.lock().unwrap();
        for status in [
            ExcelCallbackStatus::Success,
            ExcelCallbackStatus::Failed(XLRET_FAILED),
            ExcelCallbackStatus::Abort,
            ExcelCallbackStatus::Uncalced,
        ] {
            FREE_CALLS.store(0, Ordering::Relaxed);
            let mut result =
                CallbackResult::callback_for_test(XLOPER12::integer(7), status, successful_free);
            assert_eq!(result.readable().is_ok(), !status.is_terminal());
            result.release().unwrap();
            assert_eq!(FREE_CALLS.load(Ordering::Relaxed), 1);
        }
    }

    #[test]
    fn miri_decoder_panic_drops_readable_owner_once() {
        let _test_guard = TEST_LOCK.lock().unwrap();
        FREE_CALLS.store(0, Ordering::Relaxed);
        let thread = std::thread::spawn(|| {
            let mut result = CallbackResult::callback_for_test(
                XLOPER12::integer(7),
                ExcelCallbackStatus::Success,
                successful_free,
            );
            let _readable = result.readable().unwrap();
            panic!("decoder panic");
        });
        assert!(thread.join().is_err());
        assert_eq!(FREE_CALLS.load(Ordering::Relaxed), 1);
    }

    #[test]
    #[allow(
        clippy::mem_forget,
        reason = "verify abandoned cleanup keeps admission pinned"
    )]
    fn miri_forgotten_value_retains_its_module_permit() {
        use crate::callback_gate::{ModuleCallbackAdmission, ModuleCallbackLifecycle};
        static GATE: ModuleCallbackAdmission =
            ModuleCallbackAdmission::new(ModuleCallbackLifecycle::Open);
        let _test_guard = TEST_LOCK.lock().unwrap();
        FREE_CALLS.store(0, Ordering::Relaxed);
        let session = HostCallbackSession::new();
        let permit = GATE.enter_for_test().unwrap();
        let value = ExcelCallbackValue {
            owner: CallbackOwner::from_callback_for_test_with_permit(
                XLOPER12::integer(7),
                ExcelCallbackStatus::Success,
                successful_free,
                &session,
                Some(permit),
            ),
        };
        std::mem::forget(value);
        GATE.close();
        assert_eq!(GATE.active(), 1);
        assert_eq!(GATE.lifecycle(), ModuleCallbackLifecycle::Closing);
        assert_eq!(FREE_CALLS.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn miri_panicking_xlfree_is_never_retried() {
        unsafe fn panicking_free(_: &mut XLOPER12) -> i32 {
            FREE_CALLS.fetch_add(1, Ordering::Relaxed);
            panic!("release panic");
        }
        let _test_guard = TEST_LOCK.lock().unwrap();
        FREE_CALLS.store(0, Ordering::Relaxed);
        let thread = std::thread::spawn(|| {
            let value = CallbackResult::callback_for_test(
                XLOPER12::integer(7),
                ExcelCallbackStatus::Success,
                panicking_free,
            );
            let _ = value.release();
        });
        assert!(thread.join().is_err());
        assert_eq!(FREE_CALLS.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn miri_consumed_callback_cleanup_failure_cannot_retry() {
        let _test_guard = TEST_LOCK.lock().unwrap();
        FREE_CALLS.store(0, Ordering::Relaxed);
        let result = CallbackResult::callback_for_test(
            XLOPER12::integer(7),
            ExcelCallbackStatus::Success,
            failed_free,
        );
        let error = result.release().unwrap_err();
        assert!(matches!(error, XllError::ExcelApi { .. }));
        assert_eq!(
            FREE_CALLS.load(Ordering::Relaxed),
            1,
            "consumed owner's Drop must not call xlFree again"
        );
    }

    #[test]
    #[allow(
        clippy::mem_forget,
        reason = "verify safe abandonment cannot issue cleanup or a success receipt"
    )]
    fn miri_forgotten_callback_result_does_not_run_cleanup() {
        let _test_guard = TEST_LOCK.lock().unwrap();
        FREE_CALLS.store(0, Ordering::Relaxed);
        // The fake integer callback has no actual allocation; only its one
        // cleanup obligation is deliberately retained by safe forget.
        let result = CallbackResult::callback_for_test(
            XLOPER12::integer(7),
            ExcelCallbackStatus::Success,
            successful_free,
        );
        std::mem::forget(result);
        assert_eq!(FREE_CALLS.load(Ordering::Relaxed), 0);
    }
    use crate::host_callback::HostCallbackSession;
    use static_assertions::assert_not_impl_any;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use xlfn_sys::{XLRET_ABORT, XLRET_FAILED, XLRET_SUCCESS, XLRET_UNCALCED};

    assert_not_impl_any!(ExcelCallbackValue<'static>: Send, Sync);
    assert_not_impl_any!(CleanupOnlyValue<'static>: Send, Sync);
    assert_not_impl_any!(CallbackOwner<'static>: Send, Sync);

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
            let mut value = CallbackOwner::from_callback_for_test(
                XLOPER12::integer(1),
                ExcelCallbackStatus::from_raw(status),
                successful_free,
            );
            assert!(matches!(
                value.state,
                CallbackValueReleaseState::TerminalSuppressed { .. }
            ));
            value.release_in_place().unwrap();
            assert_eq!(FREE_CALLS.load(Ordering::Relaxed), 1);
            drop(value);
            assert_eq!(FREE_CALLS.load(Ordering::Relaxed), 1);
        }
    }

    #[test]
    fn indeterminate_cleanup_is_attempted_once_and_never_retried() {
        let _test_guard = TEST_LOCK.lock().unwrap();
        FREE_CALLS.store(0, Ordering::Relaxed);
        let mut value = CallbackOwner::from_callback_for_test(
            XLOPER12::integer(1),
            ExcelCallbackStatus::Success,
            failed_free,
        );

        assert!(value.release_in_place().is_err());
        assert_eq!(FREE_CALLS.load(Ordering::Relaxed), 1);
        assert!(matches!(
            value.state,
            CallbackValueReleaseState::Indeterminate {
                status: ExcelCallbackStatus::Failed(XLRET_FAILED)
            }
        ));
        assert!(value.release_in_place().is_err());
        drop(value);
        assert_eq!(FREE_CALLS.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn xlf_register_release_abort_suppresses_unregister() {
        let _test_guard = TEST_LOCK.lock().unwrap();
        let script = crate::registration::host::test_support::CallbackScript::install([]);
        FREE_CALLS.store(0, Ordering::Relaxed);
        let session = HostCallbackSession::new();
        let mut register_result = CallbackOwner::from_callback_for_test_with_session(
            XLOPER12::number(42.0),
            ExcelCallbackStatus::Success,
            aborting_free,
            &session,
        );
        assert!(register_result.release_in_place().is_err());

        // SAFETY: terminal suppression prevents this argument-free call from
        // reaching the script, which deliberately has no replies.
        let suppressed = unsafe { session.call(xlfn_sys::XLF_UNREGISTER, &[]) }
            .err()
            .expect("aborted release must suppress unregister");
        assert_eq!(suppressed.status, ExcelCallbackStatus::Abort);
        script.assert_calls(&[]);
        assert_eq!(FREE_CALLS.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn xlf_unregister_release_uncalced_suppresses_set_name() {
        let _test_guard = TEST_LOCK.lock().unwrap();
        let script = crate::registration::host::test_support::CallbackScript::install([]);
        FREE_CALLS.store(0, Ordering::Relaxed);
        let session = HostCallbackSession::new();
        let mut unregister_result = CallbackOwner::from_callback_for_test_with_session(
            XLOPER12::boolean(true),
            ExcelCallbackStatus::Success,
            uncalced_free,
            &session,
        );
        assert!(unregister_result.release_in_place().is_err());

        // SAFETY: terminal suppression prevents this argument-free call from
        // reaching the script, which deliberately has no replies.
        let suppressed = unsafe { session.call(xlfn_sys::XLF_SET_NAME, &[]) }
            .err()
            .expect("uncalced release must suppress name deletion");
        assert_eq!(suppressed.status, ExcelCallbackStatus::Uncalced);
        script.assert_calls(&[]);
        assert_eq!(FREE_CALLS.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn xlf_caller_value_drop_is_allowed_after_nested_terminal_callback() {
        let _test_guard = TEST_LOCK.lock().unwrap();
        FREE_CALLS.store(0, Ordering::Relaxed);
        let session = HostCallbackSession::new();
        let caller = CallbackOwner::from_callback_for_test_with_session(
            XLOPER12::integer(1),
            ExcelCallbackStatus::Success,
            successful_free,
            &session,
        );
        session.suppress_for_test(ExcelCallbackStatus::Abort);
        drop(caller);
        assert_eq!(FREE_CALLS.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn terminal_xl_free_allows_cleanup_of_other_live_values() {
        let _test_guard = TEST_LOCK.lock().unwrap();
        FREE_CALLS.store(0, Ordering::Relaxed);
        let session = HostCallbackSession::new();
        let mut first = CallbackOwner::from_callback_for_test_with_session(
            XLOPER12::integer(1),
            ExcelCallbackStatus::Success,
            aborting_free,
            &session,
        );
        let second = CallbackOwner::from_callback_for_test_with_session(
            XLOPER12::integer(2),
            ExcelCallbackStatus::Success,
            successful_free,
            &session,
        );
        let third = CallbackOwner::from_callback_for_test_with_session(
            XLOPER12::integer(3),
            ExcelCallbackStatus::Success,
            successful_free,
            &session,
        );

        assert!(first.release_in_place().is_err());
        assert_eq!(session.terminal_status(), Some(ExcelCallbackStatus::Abort));
        drop(first);
        drop(second);
        drop(third);
        assert_eq!(FREE_CALLS.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn callback_value_holding_module_permit_keeps_gate_closing_until_released() {
        use crate::callback_gate::ModuleCallbackLifecycle;
        static ACTIVE_DURING_FREE: AtomicUsize = AtomicUsize::new(usize::MAX);
        unsafe fn observe_free(_: &mut XLOPER12) -> i32 {
            ACTIVE_DURING_FREE.store(
                crate::module_runtime::global()
                    .callback_admission()
                    .active(),
                Ordering::Relaxed,
            );
            XLRET_SUCCESS
        }
        let _callback_guard = crate::test_callback::lock();
        let _test_guard = TEST_LOCK.lock().unwrap();
        crate::module_runtime::reset_callbacks_for_test();
        ACTIVE_DURING_FREE.store(usize::MAX, Ordering::Relaxed);
        let permit = crate::callback_gate::enter_callback().expect("open gate admits");
        let session = HostCallbackSession::new();
        let mut value = CallbackOwner::from_callback_for_test_with_permit(
            XLOPER12::integer(42),
            ExcelCallbackStatus::Success,
            observe_free,
            &session,
            Some(permit),
        );

        crate::module_runtime::close_callbacks_for_test();
        let gate = crate::module_runtime::global().callback_admission();
        assert_eq!(gate.active(), 1);
        assert_eq!(gate.lifecycle(), ModuleCallbackLifecycle::Closing);
        // New callbacks are rejected while the gate is closing.
        assert!(crate::callback_gate::enter_callback().is_err());

        // Releasing the value releases the permit.
        assert!(value.release_in_place().is_ok());
        assert_eq!(ACTIVE_DURING_FREE.load(Ordering::Relaxed), 1);
        assert_eq!(gate.active(), 0);
        assert_eq!(gate.lifecycle(), ModuleCallbackLifecycle::Closed);

        // Gate has fully closed once the permit is dropped.
        assert!(crate::callback_gate::enter_callback().is_err());
    }
}
