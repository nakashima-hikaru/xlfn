use std::cell::Cell;
use std::sync::{Mutex, MutexGuard};

// The module gate is shared by all tests, including Windows builds without
// any optional capability. Installing a mock Excel callback is only needed
// by portable callback tests and the async/handle suites.
static CALLBACK_TEST_LOCK: Mutex<()> = Mutex::new(());

thread_local! {
    static CALLBACK_TEST_LOCK_DEPTH: Cell<usize> = const { Cell::new(0) };
}

pub(crate) struct CallbackTestGuard {
    guard: Option<MutexGuard<'static, ()>>,
}

pub(crate) fn lock() -> CallbackTestGuard {
    let reentrant = CALLBACK_TEST_LOCK_DEPTH.get() != 0;
    let guard = if reentrant {
        None
    } else {
        Some(
            CALLBACK_TEST_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    };
    CALLBACK_TEST_LOCK_DEPTH.set(CALLBACK_TEST_LOCK_DEPTH.get() + 1);
    CallbackTestGuard { guard }
}

impl Drop for CallbackTestGuard {
    fn drop(&mut self) {
        let depth = CALLBACK_TEST_LOCK_DEPTH
            .get()
            .checked_sub(1)
            .expect("callback test lock depth remains balanced");
        if depth == 0 {
            drop(self.guard.take());
        }
        CALLBACK_TEST_LOCK_DEPTH.set(depth);
    }
}

#[cfg(any(not(target_os = "windows"), feature = "async", feature = "handles"))]
pub(crate) use mock::*;

#[cfg(any(not(target_os = "windows"), feature = "async", feature = "handles"))]
mod mock {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
    use xlfn_sys::{XL_ASYNC_RETURN, XL_FREE, XLOPER12, XLRET_ABORT, XLRET_FAILED, XLRET_SUCCESS};
    #[cfg(feature = "handles")]
    use xlfn_sys::{
        XL_SHEET_ID, XL_SHEET_NM, XLF_CALLER, XLMREF12, XLOPER12MRef, XLOPER12SRef, XLOPER12Value,
        XLREF12, XLTYPE_REF, XLTYPE_SREF, XLTYPE_STR,
    };

    static TOTAL_CALLS: AtomicUsize = AtomicUsize::new(0);
    static ASYNC_RETURN_CALLS: AtomicUsize = AtomicUsize::new(0);
    static FREE_CALLS: AtomicUsize = AtomicUsize::new(0);
    static LAST_ASYNC_VALUE: AtomicI32 = AtomicI32::new(-2);
    static TERMINAL_FUNCTION: AtomicI32 = AtomicI32::new(-1);
    static TERMINAL_STATUS: AtomicI32 = AtomicI32::new(XLRET_ABORT);
    static TERMINAL_USED: AtomicBool = AtomicBool::new(false);
    static ASYNC_REJECTED: AtomicBool = AtomicBool::new(false);
    #[cfg(feature = "handles")]
    static FORMULA_CALLER_KIND: AtomicI32 = AtomicI32::new(0);
    static CALLBACK_ORDER: Mutex<Vec<i32>> = Mutex::new(Vec::new());
    #[cfg(feature = "handles")]
    static FORMULA_CALLER_REFERENCES: XLMREF12 = XLMREF12 {
        count: 1,
        reftbl: [XLREF12 {
            rw_first: 11,
            rw_last: 11,
            col_first: 3,
            col_last: 3,
        }],
    };
    #[cfg(feature = "handles")]
    static FORMULA_SHEET_NAME: [u16; 6] = [5, 83, 104, 101, 101, 116];

    #[cfg(feature = "handles")]
    pub(crate) enum FormulaCallerKind {
        Ref = 1,
        SRef = 2,
    }
    pub(crate) fn install() {
        // SAFETY: `callback` has the exact MdCallBack12 ABI and remains a
        // process-live function for the duration of the test binary.
        unsafe {
            xlfn_sys::install_callback_for_abi_probe(
                callback as *const () as *mut std::ffi::c_void,
            );
        }
    }

    pub(crate) fn reset() {
        crate::module_runtime::reset_callbacks_for_test();
        TOTAL_CALLS.store(0, Ordering::Relaxed);
        ASYNC_RETURN_CALLS.store(0, Ordering::Relaxed);
        FREE_CALLS.store(0, Ordering::Relaxed);
        LAST_ASYNC_VALUE.store(-2, Ordering::Relaxed);
        TERMINAL_FUNCTION.store(-1, Ordering::Relaxed);
        TERMINAL_STATUS.store(XLRET_ABORT, Ordering::Relaxed);
        TERMINAL_USED.store(false, Ordering::Relaxed);
        ASYNC_REJECTED.store(false, Ordering::Relaxed);
        #[cfg(feature = "handles")]
        FORMULA_CALLER_KIND.store(0, Ordering::Relaxed);
        CALLBACK_ORDER
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }

    #[cfg(not(target_os = "windows"))]
    pub(crate) fn set_terminal(function: i32, status: i32) {
        TERMINAL_FUNCTION.store(function, Ordering::Relaxed);
        TERMINAL_STATUS.store(status, Ordering::Relaxed);
        TERMINAL_USED.store(false, Ordering::Relaxed);
    }

    #[cfg(feature = "async")]
    pub(crate) fn set_async_rejected(rejected: bool) {
        ASYNC_REJECTED.store(rejected, Ordering::Relaxed);
    }

    #[cfg(feature = "handles")]
    pub(crate) fn set_formula_caller(kind: FormulaCallerKind) {
        FORMULA_CALLER_KIND.store(kind as i32, Ordering::Relaxed);
    }

    #[cfg(not(target_os = "windows"))]
    pub(crate) fn total_calls() -> usize {
        TOTAL_CALLS.load(Ordering::Relaxed)
    }

    #[cfg(feature = "async")]
    pub(crate) fn async_return_calls() -> usize {
        ASYNC_RETURN_CALLS.load(Ordering::Acquire)
    }

    pub(crate) fn free_calls() -> usize {
        FREE_CALLS.load(Ordering::Relaxed)
    }

    #[cfg(feature = "async")]
    pub(crate) fn last_async_value() -> i32 {
        LAST_ASYNC_VALUE.load(Ordering::Relaxed)
    }

    #[cfg(all(feature = "async", not(target_os = "windows")))]
    pub(crate) fn callback_order() -> Vec<i32> {
        CALLBACK_ORDER
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    #[cfg(feature = "handles")]
    pub(crate) fn calls_for(function: i32) -> usize {
        CALLBACK_ORDER
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .filter(|&&called| called == function)
            .count()
    }

    unsafe extern "system" fn callback(
        function: i32,
        argument_count: i32,
        arguments: *mut *mut XLOPER12,
        result: *mut XLOPER12,
    ) -> i32 {
        CALLBACK_ORDER
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(function);
        TOTAL_CALLS.fetch_add(1, Ordering::Relaxed);
        if function == XL_FREE {
            FREE_CALLS.fetch_add(1, Ordering::Relaxed);
            return XLRET_SUCCESS;
        }
        #[cfg(feature = "handles")]
        match (FORMULA_CALLER_KIND.load(Ordering::Relaxed), function) {
            (kind, XLF_CALLER) if kind != 0 => {
                // SAFETY: the test callback owns the static reference table for
                // the duration of the process.
                let references = std::ptr::from_ref(&FORMULA_CALLER_REFERENCES).cast_mut();
                if kind == FormulaCallerKind::Ref as i32 {
                    // SAFETY: the callback contract supplies writable result storage.
                    unsafe {
                        *result = XLOPER12 {
                            value: XLOPER12Value {
                                mref: XLOPER12MRef {
                                    references,
                                    sheet_id: 17,
                                },
                            },
                            xltype: XLTYPE_REF,
                        };
                    }
                } else {
                    // SAFETY: the callback contract supplies writable result storage.
                    unsafe {
                        *result = XLOPER12 {
                            value: XLOPER12Value {
                                sref: XLOPER12SRef {
                                    count: 1,
                                    reference: XLREF12 {
                                        rw_first: 11,
                                        rw_last: 11,
                                        col_first: 3,
                                        col_last: 3,
                                    },
                                },
                            },
                            xltype: XLTYPE_SREF,
                        };
                    }
                }
                return XLRET_SUCCESS;
            }
            (kind, XL_SHEET_NM) if kind != 0 => {
                // SAFETY: the callback contract supplies writable result storage;
                // the static counted string remains live for the test process.
                unsafe {
                    *result = XLOPER12 {
                        value: XLOPER12Value {
                            string: FORMULA_SHEET_NAME.as_ptr().cast_mut(),
                        },
                        xltype: XLTYPE_STR,
                    };
                }
                return XLRET_SUCCESS;
            }
            (kind, XL_SHEET_ID) if kind != 0 => {
                // SAFETY: the test callback owns the static reference table for
                // the duration of the process.
                let references = std::ptr::from_ref(&FORMULA_CALLER_REFERENCES).cast_mut();
                // SAFETY: the callback contract supplies writable result storage.
                unsafe {
                    *result = XLOPER12 {
                        value: XLOPER12Value {
                            mref: XLOPER12MRef {
                                references,
                                sheet_id: 19,
                            },
                        },
                        xltype: XLTYPE_REF,
                    };
                }
                return XLRET_SUCCESS;
            }
            _ => {}
        }
        if function == XL_ASYNC_RETURN {
            if ASYNC_REJECTED.load(Ordering::Relaxed) {
                ASYNC_RETURN_CALLS.fetch_add(1, Ordering::Release);
                return XLRET_FAILED;
            }
            if argument_count != 2 || arguments.is_null() || result.is_null() {
                ASYNC_RETURN_CALLS.fetch_add(1, Ordering::Release);
                return XLRET_FAILED;
            }
            // SAFETY: arguments points to an array of at least 2 pointers for xlAsyncReturn.
            let returned_slot = unsafe { arguments.add(1) };
            // SAFETY: the callback contract supplies the live argument pointer.
            let returned = unsafe { *returned_slot };
            let value = if returned.is_null() {
                -1
            } else {
                // SAFETY: `returned` is the live async result pointer.
                let returned = unsafe { &*returned };
                if returned.base_type() == xlfn_sys::XLTYPE_NUM {
                    // SAFETY: XLTYPE_NUM selects the number union member.
                    unsafe { returned.value.number as i32 }
                } else {
                    -1
                }
            };
            LAST_ASYNC_VALUE.store(value, Ordering::Release);
            ASYNC_RETURN_CALLS.fetch_add(1, Ordering::Release);
            // SAFETY: the callback contract supplies writable result storage.
            unsafe {
                *result = XLOPER12::boolean(true);
            }
            return XLRET_SUCCESS;
        }

        let terminal_function = TERMINAL_FUNCTION.load(Ordering::Relaxed);
        if function == terminal_function && !TERMINAL_USED.swap(true, Ordering::AcqRel) {
            return TERMINAL_STATUS.load(Ordering::Relaxed);
        }
        XLRET_FAILED
    }
}
