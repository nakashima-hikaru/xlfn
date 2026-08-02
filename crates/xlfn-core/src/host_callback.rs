use crate::{ExcelCallbackStatus, ExcelCallbackValue};
use std::ptr::NonNull;
use xlfn_sys::XLOPER12;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HostCallbackState {
    Available,
    Suppressed(ExcelCallbackStatus),
}

pub(crate) struct HostCallbackSession {
    state: HostCallbackState,
}

impl HostCallbackSession {
    pub(crate) const fn new() -> Self {
        Self {
            state: HostCallbackState::Available,
        }
    }

    #[must_use]
    pub(crate) const fn state(&self) -> HostCallbackState {
        self.state
    }

    #[must_use]
    pub(crate) const fn permits_callbacks(&self) -> bool {
        matches!(self.state, HostCallbackState::Available)
    }

    #[must_use]
    pub(crate) const fn terminal_status(&self) -> Option<ExcelCallbackStatus> {
        match self.state {
            HostCallbackState::Available => None,
            HostCallbackState::Suppressed(status) => Some(status),
        }
    }

    /// Calls Excel once unless a terminal status was already observed in this
    /// entrypoint. Every lifecycle callback must go through this method so a
    /// terminal result suppresses all later host calls in the same session.
    pub(crate) unsafe fn call(
        &mut self,
        function: i32,
        arguments: &[NonNull<XLOPER12>],
    ) -> Result<(ExcelCallbackStatus, ExcelCallbackValue), HostCallbackSuppressed> {
        if let HostCallbackState::Suppressed(status) = self.state {
            return Err(HostCallbackSuppressed { status });
        }

        // SAFETY: forwarded from this method's caller.
        let (raw_status, result) = unsafe { ExcelCallbackValue::call(function, arguments) };
        let status = ExcelCallbackStatus::from_raw(raw_status);
        self.observe(status);
        Ok((status, result))
    }

    fn observe(&mut self, status: ExcelCallbackStatus) {
        if status.is_terminal() {
            self.state = HostCallbackState::Suppressed(status);
        }
    }

    #[cfg(test)]
    pub(crate) fn suppress_for_test(&mut self, status: ExcelCallbackStatus) {
        self.observe(status);
    }

    #[cfg(test)]
    pub(crate) fn call_for_test(
        &mut self,
        invoke: impl FnOnce() -> ExcelCallbackStatus,
    ) -> Result<ExcelCallbackStatus, HostCallbackSuppressed> {
        if let HostCallbackState::Suppressed(status) = self.state {
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
    use std::cell::Cell;
    use xlfn_sys::{XLRET_ABORT, XLRET_UNCALCED};

    #[test]
    fn session_rejects_every_callback_after_abort() {
        let mut session = HostCallbackSession::new();
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
        let mut session = HostCallbackSession::new();
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
        let mut session = HostCallbackSession::new();
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
