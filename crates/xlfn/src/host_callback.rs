use crate::callback_value::ExcelCallbackValue;
use crate::return_abi::ExcelCallbackStatus;
use std::cell::Cell;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::rc::Rc;
use xlfn_sys::XLOPER12;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HostCallbackState {
    Available,
    Suppressed(ExcelCallbackStatus),
}

impl HostCallbackState {
    pub(crate) fn blocked_status(self) -> Option<ExcelCallbackStatus> {
        match self {
            Self::Available => None,
            Self::Suppressed(status) => Some(status),
        }
    }
}

pub(crate) struct HostCallbackSession {
    state: Cell<HostCallbackState>,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl HostCallbackSession {
    pub(crate) fn new() -> Self {
        Self {
            state: Cell::new(HostCallbackState::Available),
            _not_send_or_sync: PhantomData,
        }
    }

    #[must_use]
    pub(crate) fn permits_callbacks(&self) -> bool {
        matches!(self.state.get(), HostCallbackState::Available)
    }

    #[must_use]
    pub(crate) fn terminal_status(&self) -> Option<ExcelCallbackStatus> {
        match self.state.get() {
            HostCallbackState::Available => None,
            HostCallbackState::Suppressed(status) => Some(status),
        }
    }

    pub(crate) fn observe(&self, status: ExcelCallbackStatus) {
        if status.is_terminal() && matches!(self.state.get(), HostCallbackState::Available) {
            self.state.set(HostCallbackState::Suppressed(status));
        }
    }

    /// Calls Excel once unless a terminal status was already observed in this
    /// entrypoint. Every lifecycle callback must go through this method so a
    /// terminal result suppresses all later host calls in the same session.
    pub(crate) unsafe fn call<'session>(
        &'session self,
        function: i32,
        arguments: &[NonNull<XLOPER12>],
    ) -> Result<(ExcelCallbackStatus, ExcelCallbackValue<'session>), HostCallbackSuppressed> {
        if let Some(status) = self.state.get().blocked_status() {
            return Err(HostCallbackSuppressed { status });
        }

        // SAFETY: forwarded from this method's caller.
        let (raw_status, result) =
            unsafe { ExcelCallbackValue::call_with_session(function, arguments, self) }.map_err(
                |suppressed| HostCallbackSuppressed {
                    status: suppressed.status,
                },
            )?;
        let status = ExcelCallbackStatus::from_raw(raw_status);
        self.observe(status);
        Ok((status, result))
    }

    #[cfg(test)]
    pub(crate) fn suppress_for_test(&self, status: ExcelCallbackStatus) {
        self.observe(status);
    }

    #[cfg(test)]
    pub(crate) fn call_for_test(
        &self,
        invoke: impl FnOnce() -> ExcelCallbackStatus,
    ) -> Result<ExcelCallbackStatus, HostCallbackSuppressed> {
        if let Some(status) = self.state.get().blocked_status() {
            return Err(HostCallbackSuppressed { status });
        }
        let status = invoke();
        self.observe(status);
        Ok(status)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct HostCallbackSuppressed {
    pub(crate) status: ExcelCallbackStatus,
}

#[cfg(test)]
mod tests {
    use super::*;
    use static_assertions::assert_not_impl_any;
    use std::cell::Cell;
    use xlfn_sys::{XLRET_ABORT, XLRET_UNCALCED};

    assert_not_impl_any!(HostCallbackSession: Send, Sync);

    #[test]
    fn session_rejects_every_callback_after_abort() {
        let session = HostCallbackSession::new();
        session.suppress_for_test(ExcelCallbackStatus::from_raw(XLRET_ABORT));

        assert!(!session.permits_callbacks());
        assert_eq!(session.terminal_status(), Some(ExcelCallbackStatus::Abort));
        // SAFETY: this test intentionally supplies a dummy operation and
        // verifies that terminal suppression prevents it from being invoked.
        let suppressed = match unsafe { session.call(123, &[]) } {
            Ok(_) => panic!("terminal session unexpectedly invoked callback"),
            Err(suppressed) => suppressed,
        };
        assert_eq!(suppressed.status, ExcelCallbackStatus::Abort);
    }

    #[test]
    fn session_rejects_every_callback_after_uncalced() {
        let session = HostCallbackSession::new();
        session.suppress_for_test(ExcelCallbackStatus::from_raw(XLRET_UNCALCED));

        assert!(!session.permits_callbacks());
        assert_eq!(
            session.terminal_status(),
            Some(ExcelCallbackStatus::Uncalced)
        );
        // SAFETY: this test intentionally supplies a dummy operation and
        // verifies that terminal suppression prevents it from being invoked.
        let suppressed = match unsafe { session.call(123, &[]) } {
            Ok(_) => panic!("terminal session unexpectedly invoked callback"),
            Err(suppressed) => suppressed,
        };
        assert_eq!(suppressed.status, ExcelCallbackStatus::Uncalced);
    }

    #[test]
    fn session_never_invokes_the_backend_after_a_terminal_callback() {
        let session = HostCallbackSession::new();
        let calls = Cell::new(0);

        session
            .call_for_test(|| {
                calls.set(calls.get() + 1);
                ExcelCallbackStatus::Abort
            })
            .unwrap();
        let result = session.call_for_test(|| {
            calls.set(calls.get() + 1);
            ExcelCallbackStatus::Success
        });

        assert_eq!(calls.get(), 1);
        assert_eq!(result.unwrap_err().status, ExcelCallbackStatus::Abort);
    }
}
