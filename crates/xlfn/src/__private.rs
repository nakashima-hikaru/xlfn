//! Private macro ABI façade for `xlfn-macros`.
//!
//! Items in this module are implementation details consumed exclusively by
//! code expanded by `#[excel_addin]`, `#[excel_function]`, and `derive` macros.
//! They are not part of the stable public API.

#[cfg(feature = "async")]
use std::future::Future;

use crate::addin::Addin;
use crate::call::with_excel_call_scope_and_state;
pub use crate::call::{CallScope, with_excel_call_scope};
#[cfg(feature = "async")]
use crate::cancellation::CancellationToken;
use crate::error::{InputError, XllError, XllResult};
use crate::lifecycle::{host_auto_close, host_auto_open, host_auto_remove};
use crate::reference::{ExcelReference, reference_from_raw};
use crate::registration::{
    FunctionVisibility, RegistrationDescriptor, RegistrationFlags, RegistrationSignature, ResultAbi,
};
#[cfg(feature = "async")]
use crate::return_value::ffi_boundary_void;
use crate::return_value::{ffi_boundary, free_return_boundary, udf_boundary_named};
use crate::runtime::Runtime;
use crate::value::ExcelCellOutput;
pub use crate::value::input::{
    ArgumentContext, ExcelParameter, argument_from_raw, argument_from_raw_with_arguments,
    argument_from_raw_with_context, cell_presence_from_raw,
};

#[doc(hidden)]
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

#[doc(hidden)]
pub use xlfn_sys::XLOPER12;

#[doc(hidden)]
pub use inventory::submit as submit_registration;

#[doc(hidden)]
pub use crate::input_identity::InputIdentityEncoder;
#[doc(hidden)]
pub use crate::registration::{ArgumentAbi, ArgumentDescriptor};
#[doc(hidden)]
pub use crate::return_value::ReturnContext;
/// Forwards the generated COM export to the internal RTD implementation.
#[doc(hidden)]
#[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
pub unsafe fn dll_get_class_object(
    class_id: *const core::ffi::c_void,
    interface_id: *const core::ffi::c_void,
    output: *mut *mut core::ffi::c_void,
) -> i32 {
    // SAFETY: the generated export forwards Excel/COM's live pointer contract.
    unsafe { crate::rtd::dll_get_class_object(class_id, interface_id, output) }
}

#[doc(hidden)]
pub fn dll_can_unload_now<A: Addin>(runtime: &'static MacroRuntime<A>) -> i32 {
    if runtime.runtime().module_residency_held() {
        1 // COM S_FALSE: the XLL still owns its physical residency lease.
    } else {
        crate::rtd::dll_can_unload_now()
    }
}
#[doc(hidden)]
pub use crate::__xlfn_private_async_exports as __xlfn_async_exports;
#[doc(hidden)]
pub use crate::__xlfn_private_async_only as __xlfn_async_only;
#[doc(hidden)]
pub use crate::utf16::utf16_eq_ignore_ascii_case;
#[doc(hidden)]
pub use crate::value::input::CellPresence;
#[doc(hidden)]
pub use crate::value::input::assert_async_parameter;
#[doc(hidden)]
pub use crate::value::output::{
    assert_async_return, assert_macro_sheet_return, assert_main_thread_return,
    assert_thread_safe_return, assert_volatile_return,
};
#[doc(hidden)]
pub use crate::value::{ExcelOutput, ExcelReturn, MainThreadReturn};

/// Asserts at compile-time that `T` implements `ExcelParameter`.
#[doc(hidden)]
pub fn assert_excel_parameter<'call, T: ExcelParameter<'call>>(_: &CallFrame<'call>) {}

/// Instantiates a [`ThreadSafeContext`](crate::addin::ThreadSafeContext) for generated UDFs.
#[doc(hidden)]
pub fn thread_safe_context<'state, A: Addin>(
    state: &'state A::State,
) -> crate::addin::ThreadSafeContext<'state, A> {
    crate::addin::ThreadSafeContext::new(state)
}

/// Instantiates a [`MainThreadContext`](crate::addin::MainThreadContext) for generated UDFs.
#[doc(hidden)]
pub fn main_thread_context<'call, A: Addin>(
    frame: &CallFrame<'call>,
    state: &'call A::State,
    runtime: &'call MacroRuntime<A>,
) -> crate::addin::MainThreadContext<'call, A> {
    crate::addin::MainThreadContext::new(state, runtime.runtime(), frame.scope)
}

/// Instantiates a [`MacroSheetContext`](crate::addin::MacroSheetContext) for generated UDFs.
#[doc(hidden)]
pub fn macro_sheet_context<'call, A: Addin>(
    frame: &CallFrame<'call>,
    state: &'call A::State,
) -> crate::addin::MacroSheetContext<'call, A> {
    crate::addin::MacroSheetContext::new(state, frame.scope)
}

#[doc(hidden)]
pub use crate::runtime::GenerationLease;

/// Instantiates an [`AsyncContext`](crate::addin::AsyncContext) for generated UDFs.
#[cfg(feature = "async")]
#[doc(hidden)]
pub fn async_context<'call, A: Addin>(
    lease: &'call crate::runtime::GenerationLease<A>,
    cancellation: &'call CancellationToken,
) -> crate::addin::AsyncContext<'call, A> {
    crate::addin::AsyncContext::new(lease.state(), cancellation)
}

/// Opaque wrapper around the add-in [`Runtime`] for generated code.
#[doc(hidden)]
pub struct MacroRuntime<A: Addin> {
    runtime: Runtime<A>,
}

impl<A: Addin> MacroRuntime<A> {
    #[doc(hidden)]
    pub const fn new() -> Self {
        Self {
            runtime: Runtime::new(),
        }
    }

    #[doc(hidden)]
    pub const fn runtime(&self) -> &Runtime<A> {
        &self.runtime
    }
}

impl<A: Addin> Default for MacroRuntime<A> {
    fn default() -> Self {
        Self::new()
    }
}

/// Compact, self-contained description of a registered Excel function emitted by `#[excel_function]`.
#[doc(hidden)]
#[derive(Clone, Copy)]
pub struct FunctionRegistration {
    export_name: &'static str,
    excel_name: &'static str,
    category: &'static str,
    description: &'static str,
    help_topic: &'static str,

    arguments: &'static [ArgumentDescriptor],
    argument_abis: &'static [ArgumentAbi],

    is_async: bool,
    thread_safe: bool,
    macro_sheet: bool,
    volatile: bool,
    hidden: bool,
}

inventory::collect!(FunctionRegistration);

impl FunctionRegistration {
    #[doc(hidden)]
    #[allow(
        clippy::too_many_arguments,
        reason = "Generated registration constructor accepts all metadata fields"
    )]
    pub const fn new(
        export_name: &'static str,
        excel_name: &'static str,
        category: &'static str,
        description: &'static str,
        help_topic: &'static str,
        arguments: &'static [ArgumentDescriptor],
        argument_abis: &'static [ArgumentAbi],
        is_async: bool,
        thread_safe: bool,
        macro_sheet: bool,
        volatile: bool,
        hidden: bool,
    ) -> Self {
        Self {
            export_name,
            excel_name,
            category,
            description,
            help_topic,
            arguments,
            argument_abis,
            is_async,
            thread_safe,
            macro_sheet,
            volatile,
            hidden,
        }
    }

    pub(crate) fn descriptor(
        &self,
        default_category: &'static str,
    ) -> XllResult<RegistrationDescriptor> {
        let category = if self.category.is_empty() {
            default_category
        } else {
            self.category
        };
        let result_abi = if self.is_async {
            ResultAbi::AsyncVoid
        } else {
            ResultAbi::Xloper
        };
        let visibility = if self.hidden {
            FunctionVisibility::Hidden
        } else {
            FunctionVisibility::Public
        };
        let flags = RegistrationFlags {
            thread_safe: self.thread_safe,
            macro_sheet: self.macro_sheet,
            volatile: self.volatile,
        };

        Ok(RegistrationDescriptor {
            export_name: self.export_name,
            excel_name: self.excel_name,
            signature: RegistrationSignature {
                result: result_abi,
                arguments: self.argument_abis,
                flags,
            },
            category,
            description: self.description,
            help_topic: self.help_topic,
            visibility,
            arguments: self.arguments,
        })
    }
}

/// Retains the quarantine if a newly acquired self-reference cannot be
/// released after an opening transaction fails.
fn release_open_residency_after_failure<A: Addin>(
    runtime: &'static MacroRuntime<A>,
    newly_acquired: bool,
) {
    if !newly_acquired {
        return;
    }
    if let Err(error) = runtime.runtime().release_module_residency() {
        crate::diagnostics::report_no_unwind("xlAutoOpen module residency release", &error);
        runtime.runtime().quarantine();
    }
}

/// Opens the add-in by registering all collected functions and initializing state.
#[doc(hidden)]
pub fn open_generated_addin<A: Addin>(
    runtime: &'static MacroRuntime<A>,
    addin_id: &'static str,
    _display_name: &'static str,
    default_category: &'static str,
    version: &'static str,
    target: &'static str,
    module_anchor: *const (),
) -> i32 {
    let newly_acquired = match runtime.runtime().ensure_module_residency(module_anchor) {
        Ok(newly_acquired) => newly_acquired,
        Err(error) => crate::lifecycle::fail_stop_module_residency(&error),
    };
    let parsed_id = match crate::diagnostics::AddinId::parse(addin_id) {
        Ok(id) => id,
        Err(_) => {
            release_open_residency_after_failure(runtime, newly_acquired);
            return 0;
        }
    };
    let mut descriptors = Vec::new();
    for registration in inventory::iter::<FunctionRegistration> {
        match registration.descriptor(default_category) {
            Ok(descriptor) => descriptors.push(descriptor),
            Err(_) => {
                release_open_residency_after_failure(runtime, newly_acquired);
                return 0;
            }
        }
    }
    descriptors.sort_unstable_by_key(|descriptor| descriptor.excel_name);
    let result = host_auto_open::<A>(runtime.runtime(), &parsed_id, version, target, &descriptors);
    if result == 0
        && newly_acquired
        && runtime.runtime().phase() != crate::lifecycle::LifecyclePhase::Quarantined
    {
        release_open_residency_after_failure(runtime, true);
    }
    result
}

/// Reports Excel's ambiguous close/deactivation hint without tearing down the runtime.
#[doc(hidden)]
pub fn auto_close_generated_addin<A: Addin>(runtime: &'static MacroRuntime<A>) -> i32 {
    host_auto_close::<A>(runtime.runtime())
}

/// Performs explicit terminal removal and unregisters all functions.
#[doc(hidden)]
pub fn auto_remove_generated_addin<A: Addin>(runtime: &'static MacroRuntime<A>) -> i32 {
    host_auto_remove::<A>(runtime.runtime())
}

/// Releases a return value pointer returned to Excel.
#[doc(hidden)]
#[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
pub unsafe fn free_generated_return(pointer: *mut xlfn_sys::XLOPER12) {
    // SAFETY: pointer is a live return block passed back by Excel to xlAutoFree12.
    let free_operation = unsafe { free_return_boundary(pointer) };
    drop(free_operation);
}

/// Supplies Add-in metadata to Excel's Add-in Manager.
#[doc(hidden)]
#[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
pub unsafe fn addin_manager_info<A: Addin>(
    runtime: &'static MacroRuntime<A>,
    display_name: &'static str,
    action: *mut xlfn_sys::XLOPER12,
) -> *mut xlfn_sys::XLOPER12 {
    ffi_boundary(runtime.runtime(), || {
        with_excel_call_scope(|call_scope| {
            // SAFETY: action is a live XLOPER12 passed by Excel.
            let action_value: f64 = unsafe { argument_from_raw(call_scope, "action", action)? };
            if action_value == 1.0 {
                Ok(display_name.to_owned())
            } else {
                Err(XllError::input("action", InputError::OutOfRange))
            }
        })
    })
}

/// Calculation call frame for converted arguments and identity recording.
#[doc(hidden)]
pub struct CallFrame<'call> {
    arguments: ArgumentContext<'call>,
    scope: &'call CallScope<'call>,
}

impl<'call> CallFrame<'call> {
    #[doc(hidden)]
    pub fn new<R: ExcelReturn, A: Addin>(
        runtime: &'call Runtime<A>,
        scope: &'call CallScope<'call>,
    ) -> Self {
        Self {
            arguments: ArgumentContext::for_return::<R, A>(runtime, scope),
            scope,
        }
    }

    #[doc(hidden)]
    pub fn return_context(&mut self, udf_id: &'static str) -> ReturnContext<'call, 'call> {
        let inputs = self.arguments.finish();
        let handles = self.arguments.take_handle_access();
        ReturnContext::for_frame(handles, udf_id, inputs)
    }

    #[doc(hidden)]
    #[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
    pub unsafe fn convert_argument<T>(
        &mut self,
        name: &'static str,
        raw: *mut xlfn_sys::XLOPER12,
    ) -> XllResult<T>
    where
        T: ExcelParameter<'call>,
    {
        // SAFETY: raw is supplied by Excel for this call.
        unsafe { argument_from_raw_with_arguments::<T>(&mut self.arguments, name, raw) }
    }

    #[doc(hidden)]
    #[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
    pub unsafe fn convert_reference(
        &mut self,
        name: &'static str,
        raw: *mut xlfn_sys::XLOPER12,
    ) -> XllResult<ExcelReference<'call>> {
        // SAFETY: raw is supplied by Excel for this call.
        unsafe { reference_from_raw(name, raw) }
    }

    #[doc(hidden)]
    #[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
    pub unsafe fn argument_presence(
        &self,
        name: &'static str,
        raw: *mut xlfn_sys::XLOPER12,
    ) -> XllResult<CellPresence> {
        // SAFETY: raw is supplied by Excel for this call.
        unsafe { cell_presence_from_raw(name, raw) }
    }
}

/// Helper free function to convert a typed argument from a raw pointer using the active call frame.
#[doc(hidden)]
#[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
pub unsafe fn convert_argument<'call, T>(
    frame: &mut CallFrame<'call>,
    name: &'static str,
    raw: *mut xlfn_sys::XLOPER12,
) -> XllResult<T>
where
    T: ExcelParameter<'call>,
{
    // SAFETY: caller guarantees raw is live for this call.
    unsafe { frame.convert_argument(name, raw) }
}

/// Helper free function to convert a reference argument from a raw pointer using the active call frame.
#[doc(hidden)]
#[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
pub unsafe fn convert_reference<'call>(
    frame: &mut CallFrame<'call>,
    name: &'static str,
    raw: *mut xlfn_sys::XLOPER12,
) -> XllResult<ExcelReference<'call>> {
    // SAFETY: caller guarantees raw is live for this call.
    unsafe { frame.convert_reference(name, raw) }
}

/// Helper free function to query cell presence for default/missing policies.
#[doc(hidden)]
#[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
pub unsafe fn argument_presence(
    frame: &CallFrame<'_>,
    name: &'static str,
    raw: *mut xlfn_sys::XLOPER12,
) -> XllResult<CellPresence> {
    // SAFETY: caller guarantees raw is live for this call.
    unsafe { frame.argument_presence(name, raw) }
}

/// Top-level synchronous UDF execution boundary.
#[doc(hidden)]
pub fn sync_udf<A: Addin, R: ExcelReturn, F>(
    runtime: &'static MacroRuntime<A>,
    udf_id: &'static str,
    excel_name: &'static str,
    execute: F,
) -> *mut xlfn_sys::XLOPER12
where
    F: for<'call> FnOnce(&'call A::State, &mut CallFrame<'call>) -> XllResult<ExcelOutput>,
{
    udf_boundary_named(runtime.runtime(), udf_id, excel_name, |state| {
        with_excel_call_scope_and_state(state, |state, scope| {
            let mut frame = CallFrame::new::<R, A>(runtime.runtime(), scope);
            execute(state, &mut frame)
        })
    })
}

/// Top-level asynchronous UDF execution boundary.
#[cfg(feature = "async")]
#[doc(hidden)]
#[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
pub unsafe fn async_udf<A, R, F, Fut>(
    runtime: &'static MacroRuntime<A>,
    udf_id: &'static str,
    excel_name: &'static str,
    async_handle: *mut xlfn_sys::XLOPER12,
    execute: F,
) where
    A: Addin,
    R: ExcelReturn + Send + 'static,
    F: for<'call> FnOnce(
        crate::runtime::GenerationLease<A>,
        CancellationToken,
        &mut CallFrame<'call>,
    ) -> XllResult<Fut>,
    Fut: Future<Output = XllResult<R>> + Send + 'static,
{
    // SAFETY: async_handle is supplied by Excel for this call.
    unsafe {
        crate::async_udf::async_udf_boundary_named(
            runtime.runtime(),
            udf_id,
            excel_name,
            async_handle,
            |lease, cancellation| {
                with_excel_call_scope(|scope| {
                    let mut frame = CallFrame::new::<R, A>(runtime.runtime(), scope);
                    let future = execute(lease, cancellation, &mut frame)?;
                    let _ = frame.arguments.finish();
                    Ok(future)
                })
            },
        )
    }
}

/// Cancels calculation in the async runtime.
#[cfg(feature = "async")]
#[doc(hidden)]
pub fn cancel_async_calculation<A: Addin>(runtime: &'static MacroRuntime<A>) {
    ffi_boundary_void(runtime.runtime(), || {
        crate::async_udf::cancel_async_calculation(runtime.runtime());
    });
}

/// Ends calculation in the async runtime.
#[cfg(feature = "async")]
#[doc(hidden)]
pub fn end_async_calculation<A: Addin>(runtime: &'static MacroRuntime<A>) {
    ffi_boundary_void(runtime.runtime(), || {
        crate::async_udf::end_async_calculation(runtime.runtime());
    });
}

/// Publishes a new handle instance inside a return context.
#[doc(hidden)]
pub fn publish_new_handle<T>(
    context: &mut ReturnContext<'_, '_>,
    operation: impl FnOnce() -> XllResult<T>,
) -> XllResult<ExcelOutput>
where
    T: crate::handle::ExcelHandleObject,
{
    context
        .publish_new_handle(operation)
        .map(|token| ExcelOutput::Scalar(ExcelCellOutput::String(token)))
}
