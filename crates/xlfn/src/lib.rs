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
//! lifecycle, value, diagnostics, and optional handle/RTD implementations.
//!
//! The `rtd` feature enables the generic RTD subscription API; `handles`
//! enables formula handles. Both features share a private Excel RTD/COM
//! transport, but neither public capability implies the other.
//!
//! The `serde` feature enables serialization of owned values and collections.
//! The `jiff` feature provides civil-date conversions for serial dates.
//!
//! # Stability boundaries
//!
//! `bench-internals` and `refinement` are unstable developer instrumentation,
//! excluded from the supported API and semantic-versioning guarantees. They may
//! be enabled by Cargo consumers, but must not be used by application code.
//! Hidden `__private` exports and dispatch hooks are likewise implementation
//! details; only documented conversion methods are customization contracts.

#![warn(missing_docs)]
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(unsafe_code)]

#[cfg(target_os = "windows")]
#[allow(
    unsafe_code,
    clippy::undocumented_unsafe_blocks,
    reason = "Windows C-ABI integration"
)]
pub(crate) mod win32;

#[doc(hidden)]
pub mod __private;
#[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
#[path = "addin.rs"]
mod addin_impl;
#[cfg(feature = "async")]
mod async_udf;
#[cfg(feature = "bench-internals")]
#[doc(hidden)]
pub mod benchmark_support;
mod boundary;
#[cfg(feature = "cache")]
#[allow(unsafe_code, reason = "Cache value arena and lease pointers")]
/// Concurrent weighted caches and typed cache registry descriptors.
pub mod cache;
#[allow(
    unsafe_code,
    reason = "Call-scoped read permits and domain witness capabilities"
)]
mod call;
mod call_return;
mod callback_gate;
#[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
mod callback_value;
#[cfg(feature = "async")]
#[allow(unsafe_code, reason = "Generational cancellation slot raw pointers")]
mod cancellation;
#[cfg(any(test, feature = "refinement"))]
mod composition_refinement;
#[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
mod crt;
/// Bounded diagnostic delivery, events, and add-in diagnostic identities.
pub mod diagnostics;
/// Worksheet errors and structured failures from add-ins and the framework.
pub mod error;
#[cfg(any(feature = "rtd", feature = "handles"))]
mod excel_rtd;
mod excel_rtd_protocol;
/// Stable execution-layer contracts and per-call metadata.
pub mod execution;
#[cfg(any(feature = "handles", all(target_os = "windows", feature = "rtd")))]
mod formula_lifetime;
mod generation;
#[cfg(feature = "handles")]
/// Typed object handles with call-scoped borrowing and generation-scoped leases.
pub mod handle;
#[allow(
    unsafe_code,
    reason = "Typed Excel host operations decode raw ABI values"
)]
mod host_api;
#[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
mod host_callback;
mod input_identity;
mod lifecycle;
#[allow(
    unsafe_code,
    reason = "Win32 module residency management requires raw FFI calls"
)]
mod module_residency;
mod module_runtime;
mod panic_boundary;
#[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
/// Borrowed worksheet references and range coordinates.
pub mod reference;
mod registration;
#[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
#[allow(
    dead_code,
    reason = "Return protocol types are consumed only at FFI boundaries"
)]
mod return_abi;
#[cfg(feature = "rtd")]
/// Typed real-time subscriptions, producers, and bounded delivery policies.
pub mod rtd;
mod runtime;
mod runtime_components;
mod shutdown;
mod shutdown_trace;
mod source_catalog;
#[cfg(any(feature = "rtd", feature = "handles"))]
mod subscription;
mod subscription_owner;
pub(crate) use source_catalog::{DynamicRtdSources, RtdSourceCatalog};
mod utf16;
/// Safe worksheet input views, owned values, and explicit output shapes.
pub mod value;

pub(crate) use xlfn_kernel::sync;

#[cfg(any(
    all(feature = "async", test),
    all(feature = "async-builtin", feature = "bench-internals")
))]
pub(crate) use addin_impl::AsyncTaskLimit;
#[cfg(all(
    feature = "async",
    any(test, all(feature = "bench-internals", not(feature = "async-builtin")))
))]
pub(crate) use addin_impl::NoAsyncExecutor;
#[cfg(all(feature = "async", test))]
pub(crate) use addin_impl::{AsyncContext, BuildInfo};
#[cfg(all(feature = "handles", test))]
pub(crate) use addin_impl::{HandleBindingLimit, HandleConfig, RuntimeConfig};
#[cfg(test)]
pub(crate) use addin_impl::{MacroSheetContext, Opened, PhysicallyUnloadableAddin};
#[cfg(any(test, feature = "bench-internals"))]
pub(crate) use addin_impl::{OpenContext, OpenResult};
#[cfg(feature = "async")]
pub(crate) use async_udf::AsyncExecutor;
#[cfg(all(feature = "async", test))]
pub(crate) use async_udf::AsyncTask;
#[cfg(all(feature = "async-builtin", any(test, feature = "bench-internals")))]
pub(crate) use async_udf::{AsyncPollerCount, BuiltinAsyncExecutor, BuiltinExecutorConfig};
pub use error::{
    ExcelApiFailure, ExcelApiFunction, ExcelCallbackStatus, ExcelError, UdfError, UdfResult,
    XllError, XllResult,
};
#[cfg(all(feature = "async", test))]
pub(crate) use shutdown::CleanupReporter;
#[cfg(feature = "rtd")]
pub(crate) use subscription_owner::BoxedRtdSubscription;
#[cfg(any(feature = "rtd", feature = "handles"))]
pub(crate) use subscription_owner::IntoRtdSubscription;
pub(crate) use subscription_owner::RtdSubscription;

mod ingress;

inventory::collect!(registration::RegistrationDescriptor);

#[cfg(test)]
#[allow(unsafe_code, reason = "Internal C-ABI raw memory access for testing")]
pub(crate) mod test_callback;

#[cfg(feature = "async")]
#[doc(hidden)]
#[macro_export]
macro_rules! __xlfn_private_async_only {
    ($($body:tt)*) => {
        $($body)*
    };
}

#[cfg(not(feature = "async"))]
#[doc(hidden)]
#[macro_export]
macro_rules! __xlfn_private_async_only {
    ($($body:tt)*) => {
        compile_error!("asynchronous Excel functions require the xlfn `async` feature");
    };
}

#[cfg(feature = "async")]
#[doc(hidden)]
#[macro_export]
macro_rules! __xlfn_private_async_exports {
    ($runtime:expr) => {
        #[used]
        #[cfg_attr(target_os = "macos", unsafe(link_section = "__DATA,.xllexp"))]
        #[cfg_attr(not(target_os = "macos"), unsafe(link_section = ".xllexp"))]
        static __XLFN_ASYNC_MANIFEST: [u8; b"__xlfn_calculation_canceled\0".len()] =
            *b"__xlfn_calculation_canceled\0";

        #[doc(hidden)]
        #[unsafe(no_mangle)]
        pub extern "system" fn __xlfn_calculation_canceled() {
            $crate::__private::v1::export_void_boundary(|| {
                $crate::__private::v1::cancel_async_calculation($runtime);
            });
        }
    };
}

#[cfg(not(feature = "async"))]
#[doc(hidden)]
#[macro_export]
macro_rules! __xlfn_private_async_exports {
    ($runtime:expr) => {};
}

#[doc(hidden)]
#[macro_export]
macro_rules! __xlfn_private_calculation_exports {
    ($runtime:expr) => {
        #[used]
        #[cfg_attr(target_os = "macos", unsafe(link_section = "__DATA,.xllexp"))]
        #[cfg_attr(not(target_os = "macos"), unsafe(link_section = ".xllexp"))]
        static __XLFN_CALCULATION_MANIFEST: [u8; b"__xlfn_calculation_ended\0".len()] =
            *b"__xlfn_calculation_ended\0";

        #[doc(hidden)]
        #[unsafe(no_mangle)]
        pub extern "system" fn __xlfn_calculation_ended() {
            $crate::__private::v1::export_void_boundary(|| {
                $crate::__private::v1::end_calculation($runtime);
            });
        }
    };
}

#[cfg(any(feature = "rtd", feature = "handles"))]
#[doc(hidden)]
#[macro_export]
macro_rules! __xlfn_private_excel_rtd_exports {
    ($runtime:expr) => {
        #[used]
        #[cfg_attr(target_os = "macos", unsafe(link_section = "__DATA,.xllexp"))]
        #[cfg_attr(not(target_os = "macos"), unsafe(link_section = ".xllexp"))]
        static __XLFN_RTD_MANIFEST: [u8; b"DllGetClassObject\0DllCanUnloadNow\0".len()] =
            *b"DllGetClassObject\0DllCanUnloadNow\0";

        #[doc(hidden)]
        #[unsafe(no_mangle)]
        pub unsafe extern "system" fn DllGetClassObject(
            __class_id: *const ::core::ffi::c_void,
            __interface_id: *const ::core::ffi::c_void,
            __output: *mut *mut ::core::ffi::c_void,
        ) -> i32 {
            $crate::__private::v1::export_status_boundary(0x8000_FFFF_u32 as i32, || {
                // SAFETY: Excel/COM supplies the three live ABI pointers for this
                // entry point, and the boundary validates their use.
                unsafe {
                    $crate::__private::v1::dll_get_class_object(
                        __class_id,
                        __interface_id,
                        __output,
                    )
                }
            })
        }

        #[doc(hidden)]
        #[unsafe(no_mangle)]
        pub extern "system" fn DllCanUnloadNow() -> i32 {
            $crate::__private::v1::export_status_boundary(1, || {
                $crate::__private::v1::dll_can_unload_now($runtime)
            })
        }
    };
}

#[cfg(not(any(feature = "rtd", feature = "handles")))]
#[doc(hidden)]
#[macro_export]
macro_rules! __xlfn_private_excel_rtd_exports {
    ($runtime:expr) => {};
}

/// Add-in state, lifecycle and execution contexts.
pub mod addin {
    pub use crate::addin_impl::{
        Addin, BuildInfo, DiagnosticsSetup, MacroSheetContext, MainThreadContext, OpenContext,
        OpenResult, Opened, PhysicallyUnloadableAddin, RuntimeConfig, ThreadSafeContext,
    };
    pub use crate::shutdown::{CleanupIssueKind, CleanupReporter};
}
pub use addin::Addin;

/// Executor resources, task admission and cooperative cancellation.
pub mod task {
    pub use crate::addin_impl::NoAsyncExecutor;
    #[cfg(feature = "async")]
    pub use crate::addin_impl::{AsyncConfig, AsyncContext, AsyncTaskLimit};
    #[cfg(feature = "async")]
    pub use crate::async_udf::{AsyncExecutor, AsyncTask};
    #[cfg(feature = "async-builtin")]
    pub use crate::async_udf::{
        AsyncPollerCount, BuiltinAsyncExecutor, BuiltinExecutorConfig, BuiltinReservation,
    };
    #[cfg(feature = "async")]
    pub use crate::cancellation::{CancellationGuarantee, CancellationToken, Cancelled};
}

/// Efficient construction of Excel array return values.
pub mod output {
    pub use crate::return_abi::{
        ExcelArrayBuilder, ExcelArrayOutput, MAX_MODULE_NUMERIC_RETURN_BYTES,
        MAX_NUMERIC_RETURN_BYTES, MAX_PROCESS_NUMERIC_RETURN_BYTES,
        MAX_THREAD_NUMERIC_RETURN_BYTES, NumericArrayBuilder, NumericArrayOutput,
        NumericReturnMemoryMetrics, numeric_return_memory_metrics,
    };
}

#[cfg(feature = "handles")]
pub use xlfn_macros::ExcelHandleObject;
pub use xlfn_macros::{ExcelEnum, ExcelInputIdentity, ExcelPrepare, excel_addin, excel_function};

/// Common imports for authoring an add-in.
pub mod prelude {
    #[cfg(feature = "handles")]
    pub use crate::ExcelHandleObject;
    pub use crate::addin::{
        Addin, MacroSheetContext, MainThreadContext, OpenContext, OpenResult, Opened,
        ThreadSafeContext,
    };
    pub use crate::error::{ExcelError, UdfError, UdfResult, XllError, XllResult};
    #[cfg(feature = "handles")]
    pub use crate::handle::{Handle, HandleAlias, HandleLease};
    pub use crate::task::NoAsyncExecutor;
    #[cfg(feature = "async")]
    pub use crate::task::{AsyncConfig, AsyncContext, AsyncTaskLimit};
    #[cfg(feature = "async")]
    pub use crate::task::{AsyncExecutor, AsyncTask};
    #[cfg(feature = "async-builtin")]
    pub use crate::task::{AsyncPollerCount, BuiltinAsyncExecutor, BuiltinExecutorConfig};
    pub use crate::value::{
        Column, ExcelArrayRef, ExcelCellRef, ExcelSerialDate, ExcelValueRef, Matrix, MatrixRef,
        NumericMatrixRef, OptionalExcelValue, Row,
    };
    pub use crate::{ExcelEnum, ExcelInputIdentity, ExcelPrepare, excel_addin, excel_function};
}
