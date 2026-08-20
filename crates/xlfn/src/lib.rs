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
//!
//! `xlfn` is the single supported Rust API for the framework. Raw ABI
//! definitions remain in `xlfn-sys`, while this crate owns the runtime,
//! lifecycle, value, handle, diagnostics, and RTD implementations.

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
pub mod addin;
#[cfg(feature = "async")]
mod async_udf;
#[cfg(feature = "bench-internals")]
#[doc(hidden)]
pub mod benchmark_support;
#[cfg_attr(
    not(feature = "unstable"),
    allow(
        dead_code,
        reason = "Unstable cache API is disabled in the stable build"
    )
)]
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
pub mod diagnostics;
pub mod error;
mod execution;
#[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
pub mod handle;
#[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
mod host_callback;
mod input_identity;
#[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
mod lifecycle;
#[doc(hidden)]
pub mod macro_support;
#[allow(
    unsafe_code,
    reason = "Win32 module residency management requires raw FFI calls"
)]
mod module_residency;
#[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
pub mod reference;
#[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
mod registration;
#[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
mod return_array;
mod return_storage;
#[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
mod return_value;
#[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
pub mod rtd;
mod runtime;
mod shutdown;
#[cfg(any(test, feature = "shutdown-refinement"))]
mod shutdown_refinement;
#[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
mod subscription;
mod utf16;
#[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
pub mod value;

#[cfg(feature = "async")]
pub use addin::AsyncContext;
pub use addin::{
    Addin, AddinMetadata, AsyncRuntimeConfig, BuildInfo, DiagnosticsSetup, MacroSheetContext,
    MainThreadContext, OpenContext, Opened, RtdConfig, RtdOpenContext, RuntimeConfig,
    ThreadSafeContext,
};
#[cfg(feature = "async")]
pub use async_udf::{async_udf_boundary_named, cancel_async_calculation, end_async_calculation};
pub use callback_value::{CallbackValueReleaseState, ExcelCallbackValue};
#[cfg(any(feature = "async", test))]
pub use cancellation::{CancellationGuarantee, CancellationToken, Cancelled};
pub use diagnostics::{
    AddinId, DiagnosticEvent, DiagnosticInitError, DiagnosticSink, DiagnosticStats, InvalidAddinId,
    diagnostic_stats,
};
pub use error::{
    DiagnosticId, DomainErrorCode, ExcelError, InputError, IntoXllError, Shape, XllError, XllResult,
};
pub use handle::{ExcelHandleObject, Handle, HandleAlias};
#[doc(hidden)]
pub use input_identity::InputIdentityEncoder;
pub use shutdown::{CleanupIssueKind, CleanupReporter};
pub mod ingress;
pub use ingress::{ExportCallGuard, ExportIngress, ExportsDrained, global_ingress};
#[doc(hidden)]
pub use lifecycle::{host_auto_close, host_auto_open, host_auto_remove};
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
pub use rtd::dll_get_class_object;

inventory::collect!(RegistrationDescriptor);

#[doc(hidden)]
pub use runtime::{CallGuard, LifecyclePhase, Runtime};
pub use subscription::{
    IntoRtdValue, RtdLimits, RtdSink, RtdSource, RtdSourceHandle, RtdSubscription, RtdTopic,
    RtdValue,
};
#[doc(hidden)]
pub use utf16::utf16_eq_ignore_ascii_case;
pub use value::{
    ArgumentContext, AsyncReturn, BoundedVarArgs, CallContext, CallScope, CellPresence, Column,
    ExcelCellOutput, ExcelCellValue, ExcelDateSystem, ExcelErrorValue, ExcelOutput, ExcelParameter,
    ExcelReturn, ExcelSerialDate, ExcelValue, FromExcel, IntoExcel, MacroSheetReturn,
    MainThreadReturn, Matrix, OptionalExcelValue, Row, ThreadSafeReturn, VolatileReturn,
    XlArrayRef, XlStrRef, XlValueRef,
};
#[doc(hidden)]
pub use value::{
    argument_from_raw, argument_from_raw_with_arguments, argument_from_raw_with_context,
    assert_async_parameter, assert_async_return, assert_excel_parameter, assert_macro_sheet_return,
    assert_main_thread_return, assert_thread_safe_return, assert_volatile_return,
    cell_presence_from_raw, with_excel_call_scope,
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

    #[cfg(feature = "async")]
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

#[cfg(feature = "async")]
#[doc(hidden)]
#[macro_export]
macro_rules! __xlfn_async_only {
    ($($body:tt)*) => {
        $($body)*
    };
}

#[cfg(not(feature = "async"))]
#[doc(hidden)]
#[macro_export]
macro_rules! __xlfn_async_only {
    ($($body:tt)*) => {
        compile_error!("asynchronous Excel functions require the xlfn `async` feature");
    };
}

#[cfg(feature = "async")]
#[doc(hidden)]
#[macro_export]
macro_rules! __xlfn_async_exports {
    ($runtime:expr) => {
        #[used]
        #[cfg_attr(target_os = "macos", unsafe(link_section = "__DATA,.xllexp"))]
        #[cfg_attr(not(target_os = "macos"), unsafe(link_section = ".xllexp"))]
        static __XLFN_ASYNC_MANIFEST: [u8;
            b"__xlfn_calculation_canceled\0__xlfn_calculation_ended\0".len()] =
            *b"__xlfn_calculation_canceled\0__xlfn_calculation_ended\0";

        #[doc(hidden)]
        #[unsafe(no_mangle)]
        pub extern "system" fn __xlfn_calculation_canceled() {
            $crate::__private::cancel_async_calculation($runtime);
        }

        #[doc(hidden)]
        #[unsafe(no_mangle)]
        pub extern "system" fn __xlfn_calculation_ended() {
            $crate::__private::end_async_calculation($runtime);
        }
    };
}

#[cfg(not(feature = "async"))]
#[doc(hidden)]
#[macro_export]
macro_rules! __xlfn_async_exports {
    ($runtime:expr) => {};
}

/// Capabilities supplied to exported functions by the generated Excel boundary.
pub mod context {
    pub use crate::addin::{MacroSheetContext, MainThreadContext, ThreadSafeContext};

    #[cfg(feature = "async")]
    pub use crate::addin::AsyncContext;
    #[cfg(feature = "async")]
    pub use crate::{CancellationGuarantee, CancellationToken, Cancelled};
}

/// Real-time data subscriptions and source registration.
pub mod rtd_api {
    pub use crate::{
        IntoRtdValue, RtdLimits, RtdSink, RtdSource, RtdSourceHandle, RtdSubscription, RtdTopic,
        RtdValue,
    };
}

/// Unstable lower-level APIs. Enable the `unstable` feature explicitly.
#[cfg(feature = "unstable")]
pub mod unstable {
    /// Calculation-scoped caches.
    pub mod cache {
        pub use crate::cache::{
            BoundCacheEndpoint, CacheEndpoint, CacheRegistry, CalculationCache, CanonicalF64,
        };
    }

    /// Exported-function execution metadata and instrumentation.
    pub mod execution {
        pub use crate::execution::{
            CalculationId, CallId, CallMetadata, CallOutcome, UdfLayer, UdfLayerGuard, UdfLayers,
            UdfResultKind,
        };
    }

    /// Explicit low-level array output construction.
    pub mod output {
        pub use crate::return_array::{XlArrayBuilder, XlArrayOutput};
    }
}

pub use xlfn_macros::{ExcelEnum, ExcelHandleObject, excel_addin, excel_function};

/// Common imports for authoring an add-in.
pub mod prelude {
    pub use crate::addin::{Addin, OpenContext, Opened, RtdOpenContext, RuntimeConfig};
    #[cfg(feature = "async")]
    pub use crate::context::AsyncContext;
    pub use crate::context::{MacroSheetContext, MainThreadContext, ThreadSafeContext};
    pub use crate::error::{ExcelError, XllError, XllResult};
    pub use crate::handle::{Handle, HandleAlias};
    pub use crate::rtd_api::{RtdSourceHandle, RtdTopic};
    pub use crate::value::{
        Column, ExcelErrorValue, ExcelSerialDate, Matrix, OptionalExcelValue, Row,
    };
    pub use crate::{ExcelEnum, ExcelHandleObject, excel_addin, excel_function};
}

/// Implementation details consumed by generated code, not a stable API.
#[doc(hidden)]
pub mod __private {
    pub const BUILD_TARGET: &str = if cfg!(all(
        target_os = "windows",
        target_arch = "x86",
        target_env = "msvc"
    )) {
        "i686-pc-windows-msvc"
    } else if cfg!(all(
        target_os = "windows",
        target_arch = "x86_64",
        target_env = "msvc"
    )) {
        "x86_64-pc-windows-msvc"
    } else {
        "unsupported-target"
    };

    pub use crate::macro_support::{
        ArgumentAbi, ArgumentDescriptor, CallFrame, CellPresence, ExcelOutput, ExcelReturn,
        FunctionRegistration, InputIdentityEncoder, MacroRuntime, MainThreadReturn, ReturnContext,
        addin_manager_info, argument_presence, assert_async_parameter, assert_async_return,
        assert_excel_parameter, assert_macro_sheet_return, assert_main_thread_return,
        assert_thread_safe_return, assert_volatile_return, auto_close_generated_addin,
        auto_remove_generated_addin, convert_argument, convert_reference, dll_can_unload_now,
        dll_get_class_object, free_generated_return, macro_sheet_context, main_thread_context,
        open_generated_addin, publish_new_handle, submit_registration, sync_udf,
        thread_safe_context, utf16_eq_ignore_ascii_case,
    };
    #[cfg(feature = "async")]
    pub use crate::macro_support::{
        GenerationLease, async_context, async_udf, cancel_async_calculation, end_async_calculation,
    };
    pub use xlfn_sys::XLOPER12;
}
