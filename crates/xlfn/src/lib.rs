//! Public facade for the xlfn framework.

#![deny(unsafe_code)]

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
#[allow(clippy::crate_in_macro_def)]
#[macro_export]
macro_rules! __xlfn_async_exports {
    () => {
        #[cfg(all(target_os = "windows", target_arch = "x86", target_env = "msvc"))]
        #[used]
        #[unsafe(link_section = ".drectve")]
        static __XLFN_ASYNC_EXPORTS: [u8; b" /EXPORT:__xlfn_calculation_canceled=___xlfn_calculation_canceled@0 /EXPORT:__xlfn_calculation_ended=___xlfn_calculation_ended@0".len()] =
            *b" /EXPORT:__xlfn_calculation_canceled=___xlfn_calculation_canceled@0 /EXPORT:__xlfn_calculation_ended=___xlfn_calculation_ended@0";

        #[used]
        #[cfg_attr(target_os = "macos", unsafe(link_section = "__DATA,.xllexp"))]
        #[cfg_attr(not(target_os = "macos"), unsafe(link_section = ".xllexp"))]
        static __XLFN_ASYNC_MANIFEST: [u8; b"__xlfn_calculation_canceled\0__xlfn_calculation_ended\0".len()] =
            *b"__xlfn_calculation_canceled\0__xlfn_calculation_ended\0";

        #[doc(hidden)]
        #[unsafe(no_mangle)]
        pub extern "system" fn __xlfn_calculation_canceled() {
            $crate::__private::ffi_boundary_void(|| {
                $crate::__private::cancel_async_calculation(&crate::__XLFN_RUNTIME);
            });
        }

        #[doc(hidden)]
        #[unsafe(no_mangle)]
        pub extern "system" fn __xlfn_calculation_ended() {
            $crate::__private::ffi_boundary_void(|| {
                $crate::__private::end_async_calculation(&crate::__XLFN_RUNTIME);
            });
        }
    };
}

#[cfg(not(feature = "async"))]
#[doc(hidden)]
#[macro_export]
macro_rules! __xlfn_async_exports {
    () => {};
}

/// Add-in lifecycle types.
pub mod addin {
    pub use xlfn_core::{
        Addin, AddinMetadata, BuildInfo, CleanupIssueKind, CleanupReporter, OpenContext,
    };
}

/// Capabilities supplied to exported functions by the generated Excel boundary.
pub mod context {
    pub use xlfn_core::{MacroSheetContext, MainThreadContext, ThreadSafeContext};

    #[cfg(feature = "async")]
    pub use xlfn_core::{AsyncContext, CancellationGuarantee, CancellationToken, Cancelled};
}

/// Conversions between Excel values and Rust types.
pub mod convert {
    pub use xlfn_core::{
        AsyncReturn, BoundedVarArgs, CallContext, CellPresence, Column, ExcelDateSystem,
        ExcelErrorValue, ExcelParameter, ExcelReference, ExcelReturn, ExcelSerialDate, FromExcel,
        FromExcelReference, IntoExcelValue, MacroSheetReturn, MainThreadReturn, Matrix,
        OptionalExcelValue, OwnedExcelValue, ReferenceArea, ReferenceAreas, ReturnContext, Row,
        SheetId, ThreadSafeReturn, VolatileReturn, XlArrayBuilder, XlArrayOutput, XlArrayRef,
        XlStrRef, XlValueRef,
    };
}

/// Formula-owned typed objects and their checked handles.
pub mod handle {
    pub use xlfn_core::{ExcelHandleObject, Handle};
}

/// Real-time data subscriptions.
pub mod rtd {
    pub use xlfn_core::{IntoRtdValue, RtdSink, RtdSource, RtdSubscription, RtdTopic, RtdValue};
}

/// Errors surfaced by add-in lifecycle, conversion, and exported functions.
pub mod error {
    pub use xlfn_core::{
        DomainErrorCode, ExcelError, InputError, IntoXllError, Shape, XllError, XllResult,
    };
}

/// Diagnostics emitted by the XLL runtime.
pub mod diagnostics {
    pub use xlfn_core::{
        DiagnosticEvent, DiagnosticInitError, DiagnosticShutdownError, DiagnosticSink,
        clear_diagnostic_sink, dropped_diagnostic_events, failed_diagnostic_writes,
        install_file_diagnostic_sink, set_diagnostic_sink,
    };
}

/// Lower-level runtime building blocks for framework integrations.
pub mod advanced {
    /// Calculation-scoped caches.
    pub mod cache {
        pub use xlfn_core::{CacheEndpoint, CacheRegistry, CalculationCache, CanonicalF64};
    }

    /// Exported-function execution metadata and instrumentation.
    pub mod execution {
        pub use xlfn_core::{
            CalculationId, CallId, CallMetadata, CallOutcome, UdfLayer, UdfLayerGuard,
            UdfResultKind,
        };
    }
}

pub use xlfn_macros::{ExcelEnum, ExcelHandleObject, excel_addin, excel_function};

/// Common imports for authoring an add-in.
pub mod prelude {
    pub use crate::addin::{Addin, CleanupIssueKind, CleanupReporter, OpenContext};
    #[cfg(feature = "async")]
    pub use crate::context::AsyncContext;
    pub use crate::context::{MacroSheetContext, MainThreadContext, ThreadSafeContext};
    pub use crate::convert::{
        BoundedVarArgs, CellPresence, Column, ExcelDateSystem, ExcelErrorValue, ExcelReference,
        ExcelSerialDate, Matrix, OptionalExcelValue, Row, XlArrayBuilder, XlArrayOutput,
        XlArrayRef, XlStrRef,
    };
    pub use crate::error::{ExcelError, IntoXllError, Shape, XllError, XllResult};
    pub use crate::handle::{ExcelHandleObject, Handle};
    pub use crate::rtd::{IntoRtdValue, RtdSink, RtdSource, RtdSubscription, RtdTopic, RtdValue};
    pub use crate::{ExcelEnum, ExcelHandleObject, excel_addin, excel_function};
}

/// Implementation details consumed by attribute macros, not a stable API.
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

    pub use inventory;
    pub use xlfn_core::{
        ArgumentAbi, ArgumentDescriptor, ExportCallGuard, ExportIngress, FunctionVisibility,
        RegistrationDescriptor, RegistrationFlags, RegistrationSignature, ResultAbi, ReturnContext,
        ReturnFreeBoundaryGuard, Runtime, argument_from_raw, argument_from_raw_with_context,
        assert_async_parameter, assert_async_return, assert_excel_parameter,
        assert_macro_sheet_return, assert_main_thread_return, assert_thread_safe_return,
        assert_volatile_return, cell_presence_from_raw, close_addin, dll_can_unload_now,
        dll_get_class_object, ffi_boundary, ffi_boundary_tracked, ffi_boundary_void,
        free_return_boundary, global_ingress, open_addin, reference_from_raw, udf_boundary_named,
        with_excel_call_scope,
    };
    #[cfg(feature = "async")]
    pub use xlfn_core::{
        async_udf_boundary_named, cancel_async_calculation, end_async_calculation,
    };
}

#[doc(hidden)]
pub mod sys {
    pub use xlfn_sys::*;
}
