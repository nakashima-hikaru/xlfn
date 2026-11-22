//! Windows COM apartment and event lifetime primitives.

use crate::win32::{
    COINIT_MULTITHREADED, COWAIT_DISPATCH_CALLS, CloseHandle, CoWaitForMultipleHandles,
    CreateEventW, E_UNEXPECTED, GetLastError, HANDLE, INFINITE, RPC_E_CHANGED_MODE, ResetEvent,
    S_FALSE, S_OK, SetEvent, WAIT_FAILED, WAIT_OBJECT_0, WaitForSingleObject,
};
use std::marker::PhantomData;
use std::os::windows::io::{AsRawHandle, BorrowedHandle};
use std::ptr;
use std::rc::Rc;

pub struct ComApartmentGuard {
    should_uninit: bool,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl ComApartmentGuard {
    pub fn enter() -> Result<Self, i32> {
        use crate::win32::CoInitializeEx;

        // SAFETY: the reserved pointer is null as required by CoInitializeEx.
        let status = unsafe { CoInitializeEx(ptr::null_mut(), COINIT_MULTITHREADED as u32) };

        match status {
            S_OK | S_FALSE => Ok(Self {
                should_uninit: true,
                _not_send_or_sync: PhantomData,
            }),
            RPC_E_CHANGED_MODE => Ok(Self {
                // The thread already owns a different COM apartment. This
                // call did not add an initialization reference to balance.
                should_uninit: false,
                _not_send_or_sync: PhantomData,
            }),
            error => Err(error),
        }
    }
}

impl Drop for ComApartmentGuard {
    fn drop(&mut self) {
        if self.should_uninit {
            use crate::win32::CoUninitialize;

            // SAFETY: `should_uninit` is set only after a successful
            // CoInitializeEx call, and this guard performs exactly one matching
            // CoUninitialize call while being used on the initializing thread.
            unsafe { CoUninitialize() };
        }
    }
}

pub fn wait_with_com_pumping(handle: BorrowedHandle<'_>, timeout_ms: u32) -> Result<(), i32> {
    // Server drains can originate on framework threads with no apartment.
    // Preserve an existing STA, and keep a new MTA initialized for the whole
    // CoWait call rather than assuming every caller arrived through COM.
    let _apartment = ComApartmentGuard::enter()?;
    let raw = handle.as_raw_handle();
    let mut index = u32::MAX;
    // SAFETY: the borrow keeps the handle live; one readable HANDLE is
    // supplied and index is writable. A classic STA dispatches incoming COM
    // calls during CoWait; COWAIT_DISPATCH_CALLS also enables them for an ASTA.
    // Omit COWAIT_DISPATCH_WINDOW_MESSAGES to avoid broad window dispatch.
    let status =
        unsafe { CoWaitForMultipleHandles(COWAIT_DISPATCH_CALLS, timeout_ms, 1, &raw, &mut index) };

    if status < 0 {
        Err(status)
    } else if index != 0 {
        Err(E_UNEXPECTED)
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Win32EventError {
    pub operation: &'static str,
    pub code: u32,
}

pub struct ManualResetEvent {
    // The generated HANDLE alias is a pointer and is therefore not Send/Sync.
    // The underlying unnamed kernel event is process-wide and safely waitable
    // from any thread, so retain its non-zero bit pattern in a plain integer.
    handle: usize,
}

impl ManualResetEvent {
    pub fn new(initial_state: bool) -> Result<Self, Win32EventError> {
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

    pub fn raw(&self) -> HANDLE {
        self.handle as HANDLE
    }

    pub fn reset(&self) -> Result<(), Win32EventError> {
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

    pub fn set(&self) -> Result<(), Win32EventError> {
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

    pub fn wait_with_com_pumping(&self) -> Result<(), i32> {
        self.wait_with_com_pumping_for(INFINITE)
    }

    pub fn wait_with_com_pumping_for(&self, timeout_ms: u32) -> Result<(), i32> {
        // SAFETY: this event uniquely owns the live handle for the borrow.
        wait_with_com_pumping(
            unsafe { BorrowedHandle::borrow_raw(self.raw()) },
            timeout_ms,
        )
    }

    pub fn wait_blocking(&self) -> Result<(), i32> {
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
