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
mod addin;
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
#[allow(
    dead_code,
    reason = "Callback release state is an internal protocol witness"
)]
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
#[cfg(any(feature = "handles", feature = "cache"))]
mod retirement_queue;
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
#[cfg(any(feature = "rtd", feature = "handles"))]
mod subscription;
mod utf16;
/// Safe worksheet input views, owned values, and explicit output shapes.
pub mod value;

pub(crate) use xlfn_kernel::sync;

pub use addin::{
    Addin, BuildInfo, DiagnosticsSetup, MacroSheetContext, MainThreadContext, NoAsyncExecutor,
    OpenContext, OpenResult, Opened, PhysicallyUnloadableAddin, RuntimeConfig, ThreadSafeContext,
};
#[cfg(feature = "async")]
pub use addin::{AsyncConfig, AsyncContext, AsyncTaskLimit};
#[cfg(feature = "handles")]
pub use addin::{HandleBindingLimit, HandleConfig};
#[cfg(feature = "rtd")]
pub use addin::{RtdConfig, RtdOpenContext};
#[cfg(feature = "async")]
pub use async_udf::{AsyncExecutor, AsyncTask};
#[cfg(feature = "async-builtin")]
pub use async_udf::{AsyncPollerCount, BuiltinAsyncExecutor, BuiltinExecutorConfig};
#[cfg(feature = "async")]
pub use cancellation::{CancellationGuarantee, CancellationToken, Cancelled};
pub use error::{
    ExcelApiFailure, ExcelApiFunction, ExcelCallbackStatus, ExcelError, XllError, XllResult,
};
#[cfg(feature = "rtd")]
pub use rtd::RtdCallContext;
pub use shutdown::{CleanupIssueKind, CleanupReporter};

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
        static __XLFN_ASYNC_MANIFEST: [u8;
            b"__xlfn_calculation_canceled\0__xlfn_calculation_ended\0".len()] =
            *b"__xlfn_calculation_canceled\0__xlfn_calculation_ended\0";

        #[doc(hidden)]
        #[unsafe(no_mangle)]
        pub extern "system" fn __xlfn_calculation_canceled() {
            $crate::__private::v1::export_void_boundary(|| {
                $crate::__private::v1::cancel_async_calculation($runtime);
            });
        }

        #[doc(hidden)]
        #[unsafe(no_mangle)]
        pub extern "system" fn __xlfn_calculation_ended() {
            $crate::__private::v1::export_void_boundary(|| {
                $crate::__private::v1::end_async_calculation($runtime);
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

/// Efficient construction of Excel array return values.
pub mod output {
    pub use crate::return_abi::{XlArrayBuilder, XlArrayOutput};
}

#[cfg(feature = "handles")]
pub use xlfn_macros::ExcelHandleObject;
pub use xlfn_macros::{ExcelEnum, excel_addin, excel_function};

/// Common imports for authoring an add-in.
pub mod prelude {
    #[cfg(feature = "handles")]
    pub use crate::ExcelHandleObject;
    pub use crate::addin::{
        Addin, MacroSheetContext, MainThreadContext, NoAsyncExecutor, OpenContext, OpenResult,
        Opened, ThreadSafeContext,
    };
    #[cfg(feature = "async")]
    pub use crate::addin::{AsyncConfig, AsyncContext, AsyncTaskLimit};
    pub use crate::error::{ExcelError, XllError, XllResult};
    #[cfg(feature = "handles")]
    pub use crate::handle::{Handle, HandleAlias, HandleLease};
    pub use crate::value::{
        Column, ExcelCellRef, ExcelSerialDate, Matrix, MatrixRef, OptionalExcelValue, Row,
    };
    #[cfg(feature = "async")]
    pub use crate::{AsyncExecutor, AsyncTask};
    #[cfg(feature = "async-builtin")]
    pub use crate::{AsyncPollerCount, BuiltinAsyncExecutor, BuiltinExecutorConfig};
    pub use crate::{ExcelEnum, excel_addin, excel_function};
}
