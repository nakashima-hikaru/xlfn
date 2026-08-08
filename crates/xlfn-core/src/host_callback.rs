use crate::callback_gate::CallbackInvocationToken;
use crate::{ExcelCallbackStatus, ExcelCallbackValue};
use std::cell::Cell;
use std::ptr::NonNull;
use std::rc::Rc;
use xlfn_sys::{XLOPER12, XLRET_FAILED};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HostCallbackState {
    Available,
    Suppressed(ExcelCallbackStatus),
    Closed,
}

pub(crate) struct HostCallbackShared {
    pub(crate) state: Cell<HostCallbackState>,
    pub(crate) invocation: CallbackInvocationToken,
}

impl HostCallbackShared {
    pub(crate) fn state(&self) -> HostCallbackState {
        self.state.get()
    }

    pub(crate) fn permits_callbacks(&self) -> bool {
        matches!(self.state.get(), HostCallbackState::Available)
    }

    pub(crate) fn terminal_status(&self) -> Option<ExcelCallbackStatus> {
        match self.state.get() {
            HostCallbackState::Available => None,
            HostCallbackState::Suppressed(status) => Some(status),
            HostCallbackState::Closed => Some(ExcelCallbackStatus::Failed(XLRET_FAILED)),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn close(&self) {
        if matches!(self.state.get(), HostCallbackState::Available) {
            self.state.set(HostCallbackState::Closed);
        }
    }
}

pub(crate) struct HostCallbackSession {
    shared: Rc<HostCallbackShared>,
}

impl HostCallbackSession {
    pub(crate) fn new() -> Self {
        Self {
            shared: Rc::new(HostCallbackShared {
                state: Cell::new(HostCallbackState::Available),
                invocation: CallbackInvocationToken::new(),
            }),
        }
    }

    #[must_use]
    pub(crate) fn state(&self) -> HostCallbackState {
        self.shared.state()
    }

    #[must_use]
    pub(crate) fn permits_callbacks(&self) -> bool {
        self.shared.permits_callbacks()
    }

    #[must_use]
    pub(crate) fn terminal_status(&self) -> Option<ExcelCallbackStatus> {
        self.shared.terminal_status()
    }

    #[allow(dead_code)]
    pub(crate) fn close(&self) {
        self.shared.close();
    }

    pub(crate) fn shared_handle(&self) -> Rc<HostCallbackShared> {
        Rc::clone(&self.shared)
    }

    /// Calls Excel once unless a terminal status was already observed in this
    /// entrypoint. Every lifecycle callback must go through this method so a
    /// terminal result suppresses all later host calls in the same session.
    pub(crate) unsafe fn call(
        &self,
        function: i32,
        arguments: &[NonNull<XLOPER12>],
    ) -> Result<(ExcelCallbackStatus, ExcelCallbackValue), HostCallbackSuppressed> {
        if let Some(status) = self.shared.state.get().blocked_status() {
            return Err(HostCallbackSuppressed { status });
        }

        // SAFETY: forwarded from this method's caller.
        let (raw_status, result) = unsafe {
            ExcelCallbackValue::call_with_session(function, arguments, self.shared_handle())
        }
        .map_err(|suppressed| HostCallbackSuppressed {
            status: suppressed.status,
        })?;
        let status = ExcelCallbackStatus::from_raw(raw_status);
        self.observe(status);
        Ok((status, result))
    }

    fn observe(&self, status: ExcelCallbackStatus) {
        observe_shared(&self.shared.state, status);
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
        if let Some(status) = self.shared.state.get().blocked_status() {
            return Err(HostCallbackSuppressed { status });
        }
        let status = invoke();
        self.observe(status);
        Ok(status)
    }
}

impl Drop for HostCallbackSession {
    fn drop(&mut self) {
        self.shared.state.set(HostCallbackState::Closed);
        self.shared.invocation.finish();
    }
}

impl HostCallbackState {
    pub(crate) fn blocked_status(self) -> Option<ExcelCallbackStatus> {
        match self {
            Self::Available => None,
            Self::Suppressed(status) => Some(status),
            Self::Closed => Some(ExcelCallbackStatus::Failed(XLRET_FAILED)),
        }
    }
}

pub(crate) fn observe_shared(state: &Cell<HostCallbackState>, status: ExcelCallbackStatus) {
    if status.is_terminal() && matches!(state.get(), HostCallbackState::Available) {
        state.set(HostCallbackState::Suppressed(status));
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct HostCallbackSuppressed {
    pub(crate) status: ExcelCallbackStatus,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use xlfn_sys::{XLRET_ABORT, XLRET_UNCALCED};

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

    #[test]
    fn scope_closes_callback_sessions_when_the_scope_ends() {
        let escaped = crate::with_excel_call_scope(|scope| scope.callbacks().shared_handle());

        assert!(!escaped.permits_callbacks());
        assert_eq!(
            escaped.terminal_status(),
            Some(ExcelCallbackStatus::Failed(XLRET_FAILED))
        );
    }

    #[allow(
        clippy::forget_non_drop,
        reason = "Intentionally testing std::mem::forget on borrowed context"
    )]
    #[test]
    fn forgetting_a_borrowed_context_does_not_keep_callbacks_open() {
        let escaped = crate::with_excel_call_scope(|scope| {
            let context = crate::MacroSheetContext::new(&(), scope);
            std::mem::forget(context);
            scope.callbacks().shared_handle()
        });

        assert_eq!(
            escaped.terminal_status(),
            Some(ExcelCallbackStatus::Failed(XLRET_FAILED))
        );
    }
}
