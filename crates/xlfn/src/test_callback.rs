// Callback fixtures and runtime open/close fixtures mutate the same module
// singleton. Both must hold the same reentrant lease for their entire lifetime:
// a separate callback mutex allows a runtime to reset a live callback permit.
pub(crate) struct CallbackTestGuard {
    _module: crate::ingress::TestModuleLease,
    // Fixture reentry belongs to the acquiring thread, unlike runtime leases
    // which may be released by a separate shutdown thread.
    _thread_bound: std::marker::PhantomData<std::rc::Rc<()>>,
}

pub(crate) fn lock() -> CallbackTestGuard {
    CallbackTestGuard {
        _module: crate::ingress::acquire_test_module_lease(),
        _thread_bound: std::marker::PhantomData,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn callback_fixture_excludes_runtime_until_last_reentrant_guard_drops() {
        let outer = super::lock();
        let inner = super::lock();
        drop(outer);
        let (started_tx, started_rx) = mpsc::channel();
        let (acquired_tx, acquired_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let _runtime = crate::ingress::acquire_test_module_lease();
            acquired_tx.send(()).unwrap();
        });
        started_rx.recv().unwrap();
        let while_callback_live = acquired_rx.recv_timeout(Duration::from_millis(50));
        drop(inner);
        worker.join().unwrap();
        assert!(matches!(
            while_callback_live,
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        acquired_rx.recv().unwrap();
    }

    #[test]
    fn runtime_fixture_excludes_callback_until_cross_thread_lease_release() {
        let runtime = crate::ingress::acquire_test_module_lease();
        // Callback setup may reenter on the runtime's opening thread.
        drop(super::lock());
        let (started_tx, started_rx) = mpsc::channel();
        let (acquired_tx, acquired_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let _callback = super::lock();
            acquired_tx.send(()).unwrap();
        });
        started_rx.recv().unwrap();
        let while_runtime_live = acquired_rx.recv_timeout(Duration::from_millis(50));
        // Runtime cleanup is allowed to release its lease on another thread.
        std::thread::spawn(move || drop(runtime)).join().unwrap();
        worker.join().unwrap();
        assert!(matches!(
            while_runtime_live,
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        acquired_rx.recv().unwrap();
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

    #[cfg(feature = "async-builtin")]
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

    #[cfg(all(feature = "async-builtin", not(target_os = "windows")))]
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
