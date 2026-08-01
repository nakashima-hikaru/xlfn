//! Safe framework primitives for Excel XLL add-ins.
//!
//! Raw pointers, Excel ownership flags, callback dispatch, and unwind barriers
//! are contained in this crate. UDF implementations consume only safe values
//! and a typed context.
//!
//! UDF completions and framework failures are emitted as structured `tracing`
//! events. This library never installs a global tracing subscriber; the XLL or
//! another host component owns subscriber configuration.

#![deny(unsafe_op_in_unsafe_fn)]
#![deny(unsafe_code)]

#[allow(unsafe_code)]
mod addin;
#[cfg(feature = "async")]
mod async_udf;
mod cache;
#[allow(unsafe_code)]
mod callback_value;
#[cfg(any(feature = "async", test))]
mod cancellation;
mod diagnostics;
mod error;
mod execution;
#[allow(unsafe_code)]
mod formula_fingerprint;
#[allow(unsafe_code)]
mod handle;
#[allow(unsafe_code)]
mod lifecycle;
#[allow(unsafe_code)]
mod reference;
#[allow(unsafe_code)]
mod registration;
#[allow(unsafe_code)]
mod return_value;
#[allow(unsafe_code)]
mod rtd;
mod runtime;
mod shutdown;
#[allow(unsafe_code)]
mod subscription;
mod utf16;
#[allow(unsafe_code)]
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
    DiagnosticEvent, DiagnosticInitError, DiagnosticShutdownError, DiagnosticSink,
    clear_diagnostic_sink, dropped_diagnostic_events, failed_diagnostic_writes,
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
    RegistryKeyDebt, ReturnContext, ReturnFreeBoundaryGuard, ffi_boundary, ffi_boundary_tracked,
    ffi_boundary_void, free_return, free_return_boundary, udf_boundary_named,
};
#[doc(hidden)]
pub use rtd::{dll_can_unload_now, dll_get_class_object};

inventory::collect!(RegistrationDescriptor);
#[doc(hidden)]
pub use runtime::{CallGuard, LifecyclePhase, Runtime};
pub use subscription::{IntoRtdValue, RtdSink, RtdSource, RtdSubscription, RtdTopic, RtdValue};
pub use value::{
    AsyncReturn, BoundedVarArgs, CallContext, CellPresence, Column, ExcelDateSystem,
    ExcelErrorValue, ExcelParameter, ExcelReturn, ExcelSerialDate, ExcelValueRef, FromExcel,
    IntoExcelValue, MacroSheetReturn, MainThreadReturn, Matrix, OptionalExcelValue,
    OwnedExcelValue, Row, ThreadSafeReturn, VolatileReturn,
};
#[doc(hidden)]
pub use value::{
    argument_from_raw, argument_from_raw_with_context, assert_async_return, assert_excel_parameter,
    assert_macro_sheet_return, assert_main_thread_return, assert_thread_safe_return,
    assert_volatile_return, cell_presence_from_raw,
};
