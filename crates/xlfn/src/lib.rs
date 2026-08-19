//! Public facade for the xlfn framework.
//!
//! Add-ins normally depend on this crate alone and use [`prelude`] together
//! with `#[excel_addin]` and `#[excel_function]`. Enable the `async` feature to
//! expose asynchronous UDF contexts and the calculation lifecycle exports.

#![forbid(unsafe_code)]

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

/// Add-in lifecycle types.
pub mod addin {
    pub use xlfn_core::{
        Addin, BuildInfo, CleanupIssueKind, CleanupReporter, OpenContext, UdfLayers,
    };
}

/// Capabilities supplied to exported functions by the generated Excel boundary.
pub mod context {
    pub use xlfn_core::{MacroSheetContext, MainThreadContext, ThreadSafeContext};

    #[cfg(feature = "async")]
    pub use xlfn_core::{AsyncContext, CancellationGuarantee, CancellationToken, Cancelled};
}

/// Worksheet values and conversion extension points.
pub mod value {
    pub use xlfn_core::{
        BoundedVarArgs, Column, ExcelCellOutput, ExcelCellValue, ExcelDateSystem, ExcelErrorValue,
        ExcelSerialDate, ExcelValue, FromExcel, IntoExcel, Matrix, OptionalExcelValue, Row,
        XlArrayRef, XlStrRef, XlValueRef,
    };
}

/// Excel reference values and their checked areas.
pub mod reference {
    pub use xlfn_core::{
        ExcelReference, FromExcelReference, ReferenceArea, ReferenceAreas, SheetId,
    };
}

/// Formula-owned typed objects and their checked handles.
pub mod handle {
    pub use xlfn_core::{ExcelHandleObject, Handle, HandleAlias};
}

/// Real-time data subscriptions.
pub mod rtd {
    pub use xlfn_core::{
        IntoRtdValue, RtdLimits, RtdSink, RtdSource, RtdSubscription, RtdTopic, RtdValue,
    };
}

/// Errors surfaced by add-in lifecycle, conversion, and exported functions.
pub mod error {
    pub use xlfn_core::{
        DiagnosticId, DomainErrorCode, ExcelError, InputError, IntoXllError, Shape, XllError,
        XllResult,
    };
}

/// Diagnostics emitted by the XLL runtime.
pub mod diagnostics {
    pub use xlfn_core::{
        AddinId, DiagnosticEvent, DiagnosticInitError, DiagnosticShutdownError, DiagnosticSink,
        InvalidAddinId, clear_diagnostic_sink, dropped_diagnostic_events, failed_diagnostic_writes,
        install_file_diagnostic_sink, set_diagnostic_sink,
    };
}

/// Lower-level runtime building blocks for framework integrations.
pub mod advanced {
    /// Calculation-scoped caches.
    pub mod cache {
        pub use xlfn_core::{
            BoundCacheEndpoint, CacheEndpoint, CacheRegistry, CalculationCache, CanonicalF64,
        };
    }

    /// Exported-function execution metadata and instrumentation.
    pub mod execution {
        pub use xlfn_core::{
            CalculationId, CallId, CallMetadata, CallOutcome, UdfLayer, UdfLayerGuard, UdfLayers,
            UdfResultKind,
        };
    }

    /// Explicit low-level array output construction.
    pub mod output {
        pub use xlfn_core::{XlArrayBuilder, XlArrayOutput};
    }
}

pub use xlfn_macros::{ExcelEnum, ExcelHandleObject, excel_addin, excel_function};

/// Common imports for authoring an add-in.
pub mod prelude {
    pub use crate::addin::{Addin, CleanupIssueKind, CleanupReporter, OpenContext, UdfLayers};
    #[cfg(feature = "async")]
    pub use crate::context::AsyncContext;
    pub use crate::context::{MacroSheetContext, MainThreadContext, ThreadSafeContext};
    pub use crate::error::{ExcelError, XllError, XllResult};
    pub use crate::handle::{Handle, HandleAlias};
    pub use crate::value::{
        Column, ExcelErrorValue, ExcelSerialDate, Matrix, OptionalExcelValue, Row,
    };
    pub use crate::{ExcelEnum, ExcelHandleObject, excel_addin, excel_function};
}

/// Implementation details consumed by generated code, not a stable API.
///
/// Items in this module are not part of xlfn's supported public API and may
/// change without notice.
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

    pub use xlfn_core::macro_support::{
        ArgumentAbi, ArgumentDescriptor, CallFrame, CellPresence, ExcelOutput, ExcelReturn,
        FunctionRegistration, InputIdentityEncoder, MacroRuntime, MainThreadReturn, ReturnContext,
        addin_manager_info, argument_presence, assert_async_parameter, assert_async_return,
        assert_excel_parameter, assert_macro_sheet_return, assert_main_thread_return,
        assert_thread_safe_return, assert_volatile_return, close_generated_addin, convert_argument,
        convert_reference, dll_can_unload_now, dll_get_class_object, free_generated_return,
        macro_sheet_context, main_thread_context, open_generated_addin, publish_new_handle,
        submit_registration, sync_udf, thread_safe_context, utf16_eq_ignore_ascii_case,
    };
    #[cfg(feature = "async")]
    pub use xlfn_core::macro_support::{
        GenerationLease, async_context, async_udf, cancel_async_calculation, end_async_calculation,
    };
    pub use xlfn_sys::XLOPER12;
}
