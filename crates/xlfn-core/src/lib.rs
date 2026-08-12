//! Safe framework primitives for Excel XLL add-ins.
//!
//! Raw pointers, Excel ownership flags, callback dispatch, and unwind barriers
//! are contained in this crate. UDF implementations consume only safe values
//! and a typed context.
//!
//! UDF completions and framework failures are emitted as structured `tracing`
//! events. This library never installs a global tracing subscriber; the XLL or
//! another host component owns subscriber configuration.
//!
//! Enable the `async` feature to include the calculation-scoped async UDF
//! executor and cancellation protocol. The default build contains the
//! synchronous runtime and the same FFI-safe lifecycle primitives.

#![deny(unsafe_op_in_unsafe_fn)]
#![deny(unsafe_code)]

#[cfg(target_os = "windows")]
#[allow(
    unsafe_code,
    clippy::undocumented_unsafe_blocks,
    reason = "Windows C-ABI integration"
)]
pub(crate) mod win32;

#[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
mod addin;
#[cfg(feature = "async")]
mod async_udf;
#[cfg(feature = "bench-internals")]
#[doc(hidden)]
pub mod benchmark_support;
mod cache;
mod callback_gate;
#[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
mod callback_value;
#[cfg(any(feature = "async", test))]
mod cancellation;
#[cfg(any(test, feature = "shutdown-refinement"))]
mod composition_refinement;
#[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
mod crt;
mod diagnostics;
mod error;
mod execution;
#[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
mod formula_fingerprint;
#[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
mod handle;
#[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
mod host_callback;
#[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
mod lifecycle;
#[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
mod reference;
#[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
mod registration;
mod return_storage;
#[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
mod return_value;
#[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
mod rtd;
mod runtime;
mod shutdown;
#[cfg(any(test, feature = "shutdown-refinement"))]
mod shutdown_refinement;
#[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
mod subscription;
mod utf16;
#[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
mod value;

#[cfg(feature = "async")]
pub use addin::AsyncContext;
pub use addin::{
    Addin, AddinMetadata, BuildInfo, MacroSheetContext, MainThreadContext, OpenContext,
    ThreadSafeContext,
};
#[cfg(feature = "async")]
pub use async_udf::{async_udf_boundary_named, cancel_async_calculation, end_async_calculation};
pub use cache::{CacheEndpoint, CacheRegistry, CalculationCache, CanonicalF64};
pub use callback_value::{CallbackValueReleaseState, ExcelCallbackValue};
#[cfg(any(feature = "async", test))]
pub use cancellation::{CancellationGuarantee, CancellationToken, Cancelled};
pub use diagnostics::{
    AddinId, DiagnosticEvent, DiagnosticInitError, DiagnosticShutdownError, DiagnosticSink,
    InvalidAddinId, clear_diagnostic_sink, dropped_diagnostic_events, failed_diagnostic_writes,
    install_file_diagnostic_sink, set_diagnostic_sink,
};
pub use error::{
    DomainErrorCode, ExcelError, InputError, IntoXllError, Shape, XllError, XllResult,
};
pub use execution::{
    CalculationId, CallId, CallMetadata, CallOutcome, UdfLayer, UdfLayerGuard, UdfResultKind,
};
pub use handle::{ExcelHandleObject, Handle};
pub use shutdown::{CleanupIssueKind, CleanupReporter};
pub mod ingress;
pub use ingress::{ExportCallGuard, ExportIngress, ExportsDrained, global_ingress};
#[doc(hidden)]
pub use lifecycle::{close_addin, open_addin};
#[doc(hidden)]
pub use reference::reference_from_raw;
pub use reference::{ExcelReference, FromExcelReference, ReferenceArea, ReferenceAreas, SheetId};
#[doc(hidden)]
pub use registration::{
    ArgumentAbi, ArgumentDescriptor, FunctionVisibility, MAX_EXCEL_FUNCTION_ARGUMENTS,
    MAX_REGISTER_ARGUMENT_HELP_ENTRIES, RegistrationDescriptor, RegistrationFlags, RegistrationId,
    RegistrationSignature, ResultAbi,
};
#[doc(hidden)]
pub use return_value::{
    CallbackCleanupDebt, CleanupDebtSet, ExcelCallbackStatus, GitCookieDebt, RegistrationDebt,
    RegistryKeyDebt, ReturnContext, ReturnFreeBoundaryGuard, ffi_boundary, ffi_boundary_void,
    free_return, free_return_boundary, udf_boundary_named,
};
#[doc(hidden)]
pub use rtd::{dll_can_unload_now, dll_get_class_object};

inventory::collect!(RegistrationDescriptor);

#[doc(hidden)]
pub use runtime::{CallGuard, LifecyclePhase, Runtime};
pub use subscription::{
    IntoRtdValue, RtdLimits, RtdSink, RtdSource, RtdSubscription, RtdTopic, RtdValue,
};
#[doc(hidden)]
pub use utf16::utf16_eq_ignore_ascii_case;
pub use value::{
    AsyncReturn, BoundedVarArgs, CallContext, CallScope, CellPresence, Column, ExcelDateSystem,
    ExcelErrorValue, ExcelParameter, ExcelReturn, ExcelSerialDate, FromExcel, IntoExcelValue,
    MacroSheetReturn, MainThreadReturn, Matrix, OptionalExcelValue, OwnedExcelValue, Row,
    ThreadSafeReturn, VolatileReturn, XlArrayBuilder, XlArrayOutput, XlArrayRef, XlStrRef,
    XlValueRef,
};
#[doc(hidden)]
pub use value::{
    argument_from_raw, argument_from_raw_with_context, assert_async_parameter, assert_async_return,
    assert_excel_parameter, assert_macro_sheet_return, assert_main_thread_return,
    assert_thread_safe_return, assert_volatile_return, cell_presence_from_raw,
    with_excel_call_scope,
};

#[cfg(test)]
#[allow(unsafe_code, reason = "Internal C-ABI raw memory access for testing")]
pub(crate) mod test_callback {
    use std::cell::Cell;
    use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
    use std::sync::{Mutex, MutexGuard, TryLockError};
    use xlfn_sys::{
        XL_ASYNC_RETURN, XL_FREE, XL_SHEET_ID, XL_SHEET_NM, XLF_CALLER, XLMREF12, XLOPER12,
        XLOPER12MRef, XLOPER12SRef, XLOPER12Value, XLREF12, XLRET_ABORT, XLRET_FAILED,
        XLRET_SUCCESS, XLTYPE_REF, XLTYPE_SREF, XLTYPE_STR,
    };

    static TOTAL_CALLS: AtomicUsize = AtomicUsize::new(0);
    static ASYNC_RETURN_CALLS: AtomicUsize = AtomicUsize::new(0);
    static FREE_CALLS: AtomicUsize = AtomicUsize::new(0);
    static LAST_ASYNC_VALUE: AtomicI32 = AtomicI32::new(-2);
    static TERMINAL_FUNCTION: AtomicI32 = AtomicI32::new(-1);
    static TERMINAL_STATUS: AtomicI32 = AtomicI32::new(XLRET_ABORT);
    static TERMINAL_USED: AtomicBool = AtomicBool::new(false);
    static ASYNC_REJECTED: AtomicBool = AtomicBool::new(false);
    static FORMULA_CALLER_KIND: AtomicI32 = AtomicI32::new(0);
    static CALLBACK_ORDER: Mutex<Vec<i32>> = Mutex::new(Vec::new());
    static CALLBACK_TEST_LOCK: Mutex<()> = Mutex::new(());
    static FORMULA_CALLER_REFERENCES: XLMREF12 = XLMREF12 {
        count: 1,
        reftbl: [XLREF12 {
            rw_first: 11,
            rw_last: 11,
            col_first: 3,
            col_last: 3,
        }],
    };
    static FORMULA_SHEET_NAME: [u16; 6] = [5, 83, 104, 101, 101, 116];

    pub(crate) enum FormulaCallerKind {
        Ref = 1,
        SRef = 2,
    }
    thread_local! {
        static CALLBACK_TEST_LOCK_DEPTH: Cell<usize> = const { Cell::new(0) };
    }

    pub(crate) struct CallbackTestGuard {
        guard: Option<MutexGuard<'static, ()>>,
    }

    pub(crate) fn lock() -> CallbackTestGuard {
        let reentrant = CALLBACK_TEST_LOCK_DEPTH.with(|depth| depth.get() != 0);
        let guard = if reentrant {
            None
        } else {
            Some(
                CALLBACK_TEST_LOCK
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()),
            )
        };
        CALLBACK_TEST_LOCK_DEPTH.with(|depth| depth.set(depth.get() + 1));
        CallbackTestGuard { guard }
    }

    pub(crate) fn try_lock() -> Option<CallbackTestGuard> {
        let reentrant = CALLBACK_TEST_LOCK_DEPTH.with(|depth| depth.get() != 0);
        let guard = if reentrant {
            None
        } else {
            match CALLBACK_TEST_LOCK.try_lock() {
                Ok(guard) => Some(guard),
                Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
                Err(TryLockError::WouldBlock) => return None,
            }
        };
        CALLBACK_TEST_LOCK_DEPTH.with(|depth| depth.set(depth.get() + 1));
        Some(CallbackTestGuard { guard })
    }

    impl Drop for CallbackTestGuard {
        fn drop(&mut self) {
            let depth = CALLBACK_TEST_LOCK_DEPTH.with(|depth| {
                depth
                    .get()
                    .checked_sub(1)
                    .expect("callback test lock depth remains balanced")
            });
            if depth == 0 {
                drop(self.guard.take());
            }
            CALLBACK_TEST_LOCK_DEPTH.with(|current| current.set(depth));
        }
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
        crate::callback_gate::reset();
        TOTAL_CALLS.store(0, Ordering::Relaxed);
        ASYNC_RETURN_CALLS.store(0, Ordering::Relaxed);
        FREE_CALLS.store(0, Ordering::Relaxed);
        LAST_ASYNC_VALUE.store(-2, Ordering::Relaxed);
        TERMINAL_FUNCTION.store(-1, Ordering::Relaxed);
        TERMINAL_STATUS.store(XLRET_ABORT, Ordering::Relaxed);
        TERMINAL_USED.store(false, Ordering::Relaxed);
        ASYNC_REJECTED.store(false, Ordering::Relaxed);
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

    pub(crate) fn set_async_rejected(rejected: bool) {
        ASYNC_REJECTED.store(rejected, Ordering::Relaxed);
    }

    pub(crate) fn set_formula_caller(kind: FormulaCallerKind) {
        FORMULA_CALLER_KIND.store(kind as i32, Ordering::Relaxed);
    }

    #[cfg(not(target_os = "windows"))]
    pub(crate) fn total_calls() -> usize {
        TOTAL_CALLS.load(Ordering::Relaxed)
    }

    pub(crate) fn async_return_calls() -> usize {
        ASYNC_RETURN_CALLS.load(Ordering::Acquire)
    }

    pub(crate) fn free_calls() -> usize {
        FREE_CALLS.load(Ordering::Relaxed)
    }

    pub(crate) fn last_async_value() -> i32 {
        LAST_ASYNC_VALUE.load(Ordering::Relaxed)
    }

    #[cfg(not(target_os = "windows"))]
    pub(crate) fn callback_order() -> Vec<i32> {
        CALLBACK_ORDER
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

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
        match (FORMULA_CALLER_KIND.load(Ordering::Relaxed), function) {
            (kind, XLF_CALLER) if kind != 0 => {
                // SAFETY: the test callback owns the static reference table for
                // the duration of the process.
                let references = (&FORMULA_CALLER_REFERENCES as *const XLMREF12).cast_mut();
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
                let references = (&FORMULA_CALLER_REFERENCES as *const XLMREF12).cast_mut();
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
            // SAFETY: the callback contract supplies the two live argument
            // pointers for xlAsyncReturn.
            let returned = unsafe { *arguments.add(1) };
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
