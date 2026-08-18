//! Opaque macro support layer for `xlfn-macros`.
//!
//! Items in this module are implementation details consumed exclusively by
//! code expanded by `#[excel_addin]`, `#[excel_function]`, and `derive` macros.
//! They are not part of the stable public API.

#[cfg(feature = "async")]
use std::future::Future;

#[cfg(any(feature = "async", test))]
use crate::cancellation::CancellationToken;
use crate::error::{InputError, XllError, XllResult};
use crate::lifecycle::{close_addin, open_addin};
use crate::reference::{ExcelReference, reference_from_raw};
use crate::registration::{
    FunctionVisibility, RegistrationDescriptor, RegistrationFlags, RegistrationSignature, ResultAbi,
};
#[cfg(feature = "async")]
use crate::return_value::ffi_boundary_void;
use crate::return_value::{ffi_boundary, free_return_boundary, udf_boundary_named};
use crate::value::{
    ArgumentContext, ExcelCellOutput, ExcelParameter, argument_from_raw,
    argument_from_raw_with_arguments, cell_presence_from_raw, with_excel_call_scope,
};
use crate::{Addin, CallScope, Runtime};

#[doc(hidden)]
pub use inventory::submit as submit_registration;

#[doc(hidden)]
pub use crate::input_identity::InputIdentityEncoder;
#[doc(hidden)]
pub use crate::registration::{ArgumentAbi, ArgumentDescriptor};
#[doc(hidden)]
pub use crate::return_value::ReturnContext;
#[doc(hidden)]
pub use crate::rtd::{dll_can_unload_now, dll_get_class_object};
#[doc(hidden)]
pub use crate::utf16::utf16_eq_ignore_ascii_case;
#[doc(hidden)]
pub use crate::value::{
    CellPresence, ExcelOutput, ExcelReturn, MainThreadReturn, assert_async_parameter,
    assert_async_return, assert_macro_sheet_return, assert_main_thread_return,
    assert_thread_safe_return, assert_volatile_return,
};

/// Asserts at compile-time that `T` implements `ExcelParameter`.
#[doc(hidden)]
pub fn assert_excel_parameter<'call, T: ExcelParameter<'call>>(_: &CallFrame<'call>) {}

/// Instantiates a [`ThreadSafeContext`](crate::addin::ThreadSafeContext) for generated UDFs.
#[doc(hidden)]
pub fn thread_safe_context<'state, S>(
    state: &'state S,
) -> crate::addin::ThreadSafeContext<'state, S> {
    crate::addin::ThreadSafeContext::new(state)
}

/// Instantiates a [`MainThreadContext`](crate::addin::MainThreadContext) for generated UDFs.
#[doc(hidden)]
pub fn main_thread_context<'state, 'call, S>(
    frame: &CallFrame<'call>,
    state: &'state S,
    runtime: &'state MacroRuntime<S>,
) -> crate::addin::MainThreadContext<'state, 'call, S> {
    crate::addin::MainThreadContext::new(state, runtime.runtime(), frame.scope)
}

/// Instantiates a [`MacroSheetContext`](crate::addin::MacroSheetContext) for generated UDFs.
#[doc(hidden)]
pub fn macro_sheet_context<'state, 'call, S>(
    frame: &CallFrame<'call>,
    state: &'state S,
) -> crate::addin::MacroSheetContext<'state, 'call, S> {
    crate::addin::MacroSheetContext::new(state, frame.scope)
}

/// Instantiates an [`AsyncContext`](crate::addin::AsyncContext) for generated UDFs.
#[cfg(feature = "async")]
#[doc(hidden)]
pub fn async_context<S>(
    state: &std::sync::Arc<S>,
    cancellation: &CancellationToken,
) -> crate::addin::AsyncContext<S> {
    crate::addin::AsyncContext::new(state.clone(), cancellation.clone())
}

/// Opaque wrapper around the add-in [`Runtime`] for generated code.
#[doc(hidden)]
pub struct MacroRuntime<S> {
    runtime: Runtime<S>,
}

impl<S> MacroRuntime<S> {
    #[doc(hidden)]
    pub const fn new() -> Self {
        Self {
            runtime: Runtime::new(),
        }
    }

    #[doc(hidden)]
    pub const fn runtime(&self) -> &Runtime<S> {
        &self.runtime
    }
}

impl<S> Default for MacroRuntime<S> {
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

/// Opens the add-in by registering all collected functions and initializing state.
#[doc(hidden)]
pub fn open_generated_addin<A: Addin>(
    runtime: &'static MacroRuntime<A::State>,
    addin_id: &'static str,
    _display_name: &'static str,
    default_category: &'static str,
    version: &'static str,
    target: &'static str,
) -> i32 {
    let parsed_id = match crate::AddinId::parse(addin_id) {
        Ok(id) => id,
        Err(_) => return 0,
    };
    let mut descriptors = Vec::new();
    for registration in inventory::iter::<FunctionRegistration> {
        match registration.descriptor(default_category) {
            Ok(descriptor) => descriptors.push(descriptor),
            Err(_) => return 0,
        }
    }
    descriptors.sort_unstable_by_key(|descriptor| descriptor.excel_name);
    open_addin::<A>(runtime.runtime(), &parsed_id, version, target, &descriptors)
}

/// Closes the add-in and unregisters all functions.
#[doc(hidden)]
pub fn close_generated_addin<A: Addin>(runtime: &'static MacroRuntime<A::State>) -> i32 {
    close_addin::<A>(runtime.runtime())
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
pub unsafe fn addin_manager_info<S>(
    runtime: &'static MacroRuntime<S>,
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
    pub fn new<R: ExcelReturn, S>(
        runtime: &'call Runtime<S>,
        scope: &'call CallScope<'call>,
    ) -> Self {
        Self {
            arguments: ArgumentContext::for_return::<R, S>(runtime, scope),
            scope,
        }
    }

    #[doc(hidden)]
    pub fn return_context<S>(
        &mut self,
        runtime: &'call MacroRuntime<S>,
        udf_id: &'static str,
    ) -> ReturnContext<'call, 'call> {
        let inputs = self.arguments.finish();
        ReturnContext::for_call(runtime.runtime(), udf_id, inputs, self.scope)
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

    #[doc(hidden)]
    pub fn record_value<T: ExcelParameter<'call>>(
        &mut self,
        name: &'static str,
        value: &T,
    ) -> XllResult<()> {
        self.arguments.record_value(name, value)
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
pub fn sync_udf<S: 'static, R: ExcelReturn, F>(
    runtime: &'static MacroRuntime<S>,
    udf_id: &'static str,
    excel_name: &'static str,
    execute: F,
) -> *mut xlfn_sys::XLOPER12
where
    F: for<'call> FnOnce(&S, &mut CallFrame<'call>) -> XllResult<ExcelOutput>,
{
    udf_boundary_named(runtime.runtime(), udf_id, excel_name, |state| {
        with_excel_call_scope(|scope| {
            let mut frame = CallFrame::new::<R, S>(runtime.runtime(), scope);
            execute(state, &mut frame)
        })
    })
}

/// Top-level asynchronous UDF execution boundary.
#[cfg(feature = "async")]
#[doc(hidden)]
#[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
pub unsafe fn async_udf<S, R, F, Fut>(
    runtime: &'static MacroRuntime<S>,
    udf_id: &'static str,
    excel_name: &'static str,
    async_handle: *mut xlfn_sys::XLOPER12,
    execute: F,
) where
    S: Send + Sync + 'static,
    R: ExcelReturn + Send + 'static,
    F: for<'call> FnOnce(
        &std::sync::Arc<S>,
        &CancellationToken,
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
            |state, cancellation| {
                with_excel_call_scope(|scope| {
                    let mut frame = CallFrame::new::<R, S>(runtime.runtime(), scope);
                    let future = execute(&state, &cancellation, &mut frame)?;
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
pub fn cancel_async_calculation<S>(runtime: &'static MacroRuntime<S>) {
    ffi_boundary_void(runtime.runtime(), || {
        crate::async_udf::cancel_async_calculation(runtime.runtime());
    });
}

/// Ends calculation in the async runtime.
#[cfg(feature = "async")]
#[doc(hidden)]
pub fn end_async_calculation<S>(runtime: &'static MacroRuntime<S>) {
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
