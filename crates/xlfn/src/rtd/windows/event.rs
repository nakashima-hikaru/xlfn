use crate::win32::{
    COWAIT_DISPATCH_CALLS, CloseHandle, CoWaitForMultipleHandles, CreateEventW, E_UNEXPECTED,
    GetLastError, HANDLE, INFINITE, ResetEvent, SetEvent, WAIT_FAILED, WAIT_OBJECT_0,
    WaitForSingleObject,
};
use std::ptr;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Win32EventError {
    pub(super) operation: &'static str,
    pub(super) code: u32,
}

pub(super) struct ManualResetEvent {
    // The generated HANDLE alias is a pointer and is therefore not Send/Sync.
    // The underlying unnamed kernel event is process-wide and safely waitable
    // from any thread, so retain its non-zero bit pattern in a plain integer.
    handle: usize,
}

impl ManualResetEvent {
    pub(super) fn new(initial_state: bool) -> Result<Self, Win32EventError> {
        // SAFETY: null security attributes select the default descriptor, both
        // BOOL values are valid, and a null name requests an unnamed event.
        let handle = unsafe { CreateEventW(ptr::null(), 1, i32::from(initial_state), ptr::null()) };
        if handle.is_null() {
            // SAFETY: CreateEventW just reported failure on this thread.
            let code = unsafe { GetLastError() };
            return Err(Win32EventError {
                operation: "CreateEventW",
                code,
            });
        }

        Ok(Self {
            handle: handle as usize,
        })
    }

    pub(super) fn raw(&self) -> HANDLE {
        self.handle as HANDLE
    }

    pub(super) fn reset(&self) -> Result<(), Win32EventError> {
        // SAFETY: this RAII object owns a live manual-reset event handle.
        if unsafe { ResetEvent(self.raw()) } == 0 {
            // SAFETY: ResetEvent just reported failure on this thread.
            let code = unsafe { GetLastError() };
            Err(Win32EventError {
                operation: "ResetEvent",
                code,
            })
        } else {
            Ok(())
        }
    }

    pub(super) fn set(&self) -> Result<(), Win32EventError> {
        // SAFETY: this RAII object owns a live manual-reset event handle.
        if unsafe { SetEvent(self.raw()) } == 0 {
            // SAFETY: SetEvent just reported failure on this thread.
            let code = unsafe { GetLastError() };
            Err(Win32EventError {
                operation: "SetEvent",
                code,
            })
        } else {
            Ok(())
        }
    }

    pub(super) fn wait_with_com_pumping(&self) -> Result<(), i32> {
        let handle = self.raw();
        let mut index = u32::MAX;
        // SAFETY: `handle` remains live for this call, one readable HANDLE is
        // supplied, and `index` is writable. A classic STA dispatches incoming
        // COM calls during CoWait; COWAIT_DISPATCH_CALLS additionally enables
        // that behavior for an ASTA and is ignored by other apartment types.
        // Deliberately omit COWAIT_DISPATCH_WINDOW_MESSAGES so teardown cannot
        // run an arbitrary Windows message loop.
        let status = unsafe {
            CoWaitForMultipleHandles(
                COWAIT_DISPATCH_CALLS as u32,
                INFINITE,
                1,
                &handle,
                &mut index,
            )
        };

        if status < 0 {
            Err(status)
        } else if index != 0 {
            Err(E_UNEXPECTED)
        } else {
            Ok(())
        }
    }

    pub(super) fn wait_blocking(&self) -> Result<(), i32> {
        // SAFETY: this RAII object owns a live event handle for the complete
        // wait. A coordinator thread has no STA work to dispatch.
        let status = unsafe { WaitForSingleObject(self.raw(), INFINITE) };
        if status == WAIT_OBJECT_0 as u32 {
            Ok(())
        } else if status == WAIT_FAILED {
            // SAFETY: WaitForSingleObject just reported failure on this thread.
            Err(unsafe { GetLastError() } as i32)
        } else {
            Err(E_UNEXPECTED)
        }
    }
}

impl Drop for ManualResetEvent {
    fn drop(&mut self) {
        // SAFETY: this object uniquely owns the non-null handle returned by
        // CreateEventW and closes it exactly once after all barrier borrows end.
        let closed = unsafe { CloseHandle(self.raw()) };
        debug_assert_ne!(closed, 0, "CloseHandle failed for RTD barrier event");
    }
}
