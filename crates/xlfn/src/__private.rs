//! Private macro ABI façade for `xlfn-macros`.
//!
//! Items in this module are implementation details consumed exclusively by
//! code expanded by `#[excel_addin]`, `#[excel_function]`, and `derive` macros.
//! They are not part of the stable public API.

/// Versioned procedural-macro ABI. Generated code must name this namespace
/// explicitly so an ABI-breaking façade change is visible at the expansion
/// site instead of silently binding to a different set of symbols.
pub mod v1 {

    /// Final generated ABI boundary, including admission and guard teardown.
    /// The fallback is a detached error value that needs no runtime ownership.
    #[doc(hidden)]
    pub fn export_value_boundary(
        operation: impl FnOnce() -> *mut xlfn_sys::XLOPER12,
    ) -> *mut xlfn_sys::XLOPER12 {
        crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(operation))
            .unwrap_or_else(|_| crate::return_abi::closing_error_pointer())
    }

    /// Final generated FP12 boundary. Excel interprets null as #NUM!.
    #[doc(hidden)]
    pub fn export_numeric_boundary(
        operation: impl FnOnce() -> *mut xlfn_sys::FP12,
    ) -> *mut xlfn_sys::FP12 {
        crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(operation))
            .unwrap_or(std::ptr::null_mut())
    }

    /// Final metadata boundary, whose detached fallback is always #VALUE!.
    #[doc(hidden)]
    pub fn export_metadata_boundary(
        operation: impl FnOnce() -> *mut xlfn_sys::XLOPER12,
    ) -> *mut xlfn_sys::XLOPER12 {
        crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(operation))
            .unwrap_or_else(|_| crate::return_abi::metadata::invalid_action_pointer())
    }

    /// Final status-returning boundary for generated Excel and COM exports.
    #[doc(hidden)]
    pub fn export_status_boundary(failure: i32, operation: impl FnOnce() -> i32) -> i32 {
        crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(operation))
            .unwrap_or(failure)
    }

    /// Final void boundary, retaining custom panic payloads without invoking
    /// their destructors before returning through the non-unwinding ABI.
    #[doc(hidden)]
    pub fn export_void_boundary(operation: impl FnOnce()) {
        let _ = crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(operation));
    }

    #[cfg(feature = "async")]
    use std::future::Future;
    #[cfg(all(feature = "async", feature = "handles"))]
    use std::pin::Pin;

    use crate::addin::{Addin, PhysicallyUnloadableAddin};
    use crate::boundary::host::{host_auto_close, host_auto_open, host_auto_remove};
    pub use crate::call::{CallScope, with_excel_call_scope, with_excel_call_scope_and_state};
    #[cfg(feature = "async")]
    pub use crate::cancellation::CancellationToken;
    use crate::error::{XllError, XllResult};
    use crate::reference::{FromExcelReference, reference_from_raw};
    use crate::registration::{RegistrationDescriptor, RegistrationSignature};
    use crate::return_abi::ffi_boundary_void;
    pub use crate::return_abi::metadata::AddinMetadata;
    use crate::return_abi::{free_return_boundary, udf_boundary_named};
    use crate::runtime::Runtime;
    #[cfg(any(feature = "handles", test))]
    pub use crate::value::input::FormulaInputMode;
    pub use crate::value::input::{
        ArgumentContext, ExcelInputIdentity, ExcelParameter, InputMode, PlainInputMode,
        PreparedArgument, argument_from_raw, argument_from_raw_with_arguments,
        cell_presence_from_raw,
    };
    #[doc(hidden)]
    pub use crate::value::output::ExcelCellSink;

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
    pub use xlfn_sys::{FP12, XLOPER12};

    #[doc(hidden)]
    pub use inventory::submit as submit_registration;

    #[doc(hidden)]
    pub use crate::call_return::{NumericReturn, ReturnContext};
    #[doc(hidden)]
    pub use crate::input_identity::InputIdentityEncoder;
    #[doc(hidden)]
    pub use crate::registration::{ArgumentAbi, ArgumentDescriptor, ReturnAbi};
    /// Forwards the generated COM export to the private Excel RTD transport.
    #[cfg(any(feature = "rtd", feature = "handles"))]
    #[doc(hidden)]
    #[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
    pub unsafe fn dll_get_class_object(
        class_id: *const core::ffi::c_void,
        interface_id: *const core::ffi::c_void,
        output: *mut *mut core::ffi::c_void,
    ) -> i32 {
        // SAFETY: the generated export forwards Excel/COM's live pointer contract.
        unsafe { crate::excel_rtd::dll_get_class_object(class_id, interface_id, output) }
    }

    #[cfg(any(feature = "rtd", feature = "handles"))]
    #[doc(hidden)]
    pub fn dll_can_unload_now<A: Addin>(runtime: &'static MacroRuntime<A>) -> i32 {
        if runtime.runtime().module_residency_held() {
            1 // COM S_FALSE: the XLL still owns its physical residency lease.
        } else {
            crate::excel_rtd::dll_can_unload_now()
        }
    }
    #[doc(hidden)]
    pub use crate::__xlfn_private_async_exports as __xlfn_async_exports;
    #[doc(hidden)]
    pub use crate::__xlfn_private_async_only as __xlfn_async_only;
    pub use crate::__xlfn_private_calculation_exports as __xlfn_calculation_exports;
    #[doc(hidden)]
    pub use crate::__xlfn_private_excel_rtd_exports as __xlfn_excel_rtd_exports;
    pub use crate::call_return::ReturnPayload as ExcelOutput;
    #[doc(hidden)]
    pub use crate::call_return::assert_async_parameter;
    #[doc(hidden)]
    pub use crate::call_return::{
        AsyncReturn, ExcelReturn, ExcelReturnSealed, MacroSheetReturn, MainThreadReturn,
        ThreadSafeReturn, VolatileReturn,
    };
    #[doc(hidden)]
    pub use crate::call_return::{
        assert_async_return, assert_macro_sheet_return, assert_main_thread_return,
        assert_thread_safe_return, assert_volatile_return,
    };
    #[doc(hidden)]
    pub use crate::utf16::utf16_eq_ignore_ascii_case;
    /// Builds an enum input error while bounding decoded diagnostic storage.
    /// Invalid UTF-16 remains an input encoding error even beyond the prefix.
    #[doc(hidden)]
    pub fn unknown_enum_error(
        text: crate::value::XlStrRef<'_>,
        argument: &'static str,
        expected: &'static str,
    ) -> XllError {
        const MAX_BYTES: usize = 1024;
        let mut actual = String::new();
        let mut truncated = false;
        for character in text.chars() {
            let character = match character {
                Ok(character) => character,
                Err(_) => {
                    return XllError::input(argument, crate::error::InputError::InvalidUtf16);
                }
            };
            if !truncated && actual.len() + character.len_utf8() <= MAX_BYTES {
                actual.push(character);
            } else {
                truncated = true;
            }
        }
        if truncated {
            actual.push_str("…[truncated]");
        }
        XllError::input(
            argument,
            crate::error::InputError::UnknownEnum { actual, expected },
        )
    }
    #[doc(hidden)]
    pub use crate::value::input::CellPresence;
    pub use xlfn_common::{ExecutionKind, FunctionVisibility};

    /// Boxes a future at the narrow generated async-task boundary while
    /// preserving its inferred scope lifetime.
    #[cfg(all(feature = "async", feature = "handles"))]
    #[doc(hidden)]
    pub fn box_scoped_future<'scope, T, F>(
        _: AsyncTaskScope<'scope>,
        future: F,
    ) -> Pin<Box<dyn Future<Output = T> + Send + 'scope>>
    where
        F: Future<Output = T> + Send + 'scope,
    {
        Box::pin(future)
    }

    /// Asserts at compile-time that `T` implements `ExcelParameter`.
    #[doc(hidden)]
    pub fn assert_excel_parameter<'call, R, T>(_: &CallFrame<'call, R::InputMode>)
    where
        R: ExcelReturn,
        T: ExcelParameter<'call, R::InputMode>,
    {
    }

    /// Instantiates a [`ThreadSafeContext`](crate::addin::ThreadSafeContext) for generated UDFs.
    #[doc(hidden)]
    pub fn thread_safe_context<'call, A: Addin, M: InputMode>(
        frame: &CallFrame<'call, M>,
        state: &'call A::SharedState,
    ) -> crate::addin::ThreadSafeContext<'call, A> {
        crate::addin::ThreadSafeContext::new(state, frame.diagnostics)
    }

    /// Instantiates a [`MainThreadContext`](crate::addin::MainThreadContext) for generated UDFs.
    #[doc(hidden)]
    pub fn main_thread_context<'call, A: Addin, M: InputMode>(
        _frame: &CallFrame<'call, M>,
        state: &'call A::SharedState,
    ) -> crate::addin::MainThreadContext<'call, A> {
        #[cfg(feature = "rtd")]
        {
            crate::addin::MainThreadContext::new(
                state,
                _frame.diagnostics,
                crate::rtd::RtdCallContext::new(
                    _frame.rtd_access(),
                    crate::host_api::ExcelHost::new(_frame.callbacks),
                ),
            )
        }
        #[cfg(not(feature = "rtd"))]
        crate::addin::MainThreadContext::new(state, _frame.diagnostics)
    }

    /// Instantiates a [`MacroSheetContext`](crate::addin::MacroSheetContext) for generated UDFs.
    #[doc(hidden)]
    pub fn macro_sheet_context<'call, A: Addin, M: InputMode>(
        frame: &CallFrame<'call, M>,
        state: &'call A::SharedState,
    ) -> crate::addin::MacroSheetContext<'call, A> {
        crate::addin::MacroSheetContext::new(
            state,
            frame.diagnostics,
            crate::host_api::ExcelHost::new(frame.callbacks),
        )
    }

    #[doc(hidden)]
    pub use crate::generation::ExecutionLease;

    #[cfg(all(feature = "async", feature = "handles"))]
    #[doc(hidden)]
    pub use crate::async_udf::{AsyncTaskScope, HandleScopedBuilder};
    #[cfg(all(feature = "async", feature = "handles"))]
    #[doc(hidden)]
    pub use crate::handle::PendingHandleLease;

    /// Instantiates an [`AsyncContext`](crate::addin::AsyncContext) for generated UDFs.
    #[cfg(feature = "async")]
    #[doc(hidden)]
    pub fn async_context<'call, A: Addin>(
        lease: &'call crate::generation::ExecutionLease<A>,
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
        pub const fn new_with_physical_unload() -> Self
        where
            A: PhysicallyUnloadableAddin,
        {
            Self {
                runtime: Runtime::new_with_physical_unload(),
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
        return_abi: ReturnAbi,

        execution: ExecutionKind,
        volatile: bool,
        visibility: FunctionVisibility,
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
            return_abi: ReturnAbi,
            execution: ExecutionKind,
            volatile: bool,
            visibility: FunctionVisibility,
        ) -> Self {
            Self {
                export_name,
                excel_name,
                category,
                description,
                help_topic,
                arguments,
                argument_abis,
                return_abi,
                execution,
                volatile,
                visibility,
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
            Ok(RegistrationDescriptor {
                export_name: self.export_name,
                excel_name: self.excel_name,
                signature: RegistrationSignature {
                    execution: self.execution,
                    return_abi: self.return_abi,
                    arguments: self.argument_abis,
                    volatile: self.volatile,
                },
                category,
                description: self.description,
                help_topic: self.help_topic,
                visibility: self.visibility,
                arguments: self.arguments,
            })
        }
    }

    /// Retains the quarantine if a newly acquired self-reference cannot be
    /// released after an opening transaction fails.
    fn release_open_residency_after_failure<A: Addin>(
        runtime: &MacroRuntime<A>,
        newly_acquired: bool,
    ) {
        // Only a failure before application initialization or a completed
        // rollback may release this reference. A state-less Addin::open
        // failure is quarantined even with the physical-unload opt-in.
        if !newly_acquired
            || !runtime.runtime().physical_unload_enabled()
            || runtime.runtime().phase() != crate::lifecycle::LifecyclePhase::Closed
        {
            return;
        }
        if let Err(error) = runtime.runtime().release_module_residency() {
            crate::diagnostics::report_no_unwind("xlAutoOpen module residency release", &error);
            runtime.runtime().quarantine_runtime();
        }
    }

    /// Opens the add-in by registering all collected functions and initializing state.
    ///
    /// # Safety
    /// This must be called from Excel's `xlAutoOpen` entrypoint on its lifecycle
    /// thread, while Excel permits the lifecycle callbacks performed here.
    /// It must not be used to renew callback authority inside another invocation.
    #[doc(hidden)]
    #[allow(
        unsafe_code,
        reason = "Excel lifecycle entrypoint grants callback authority"
    )]
    pub unsafe fn open_generated_addin<A: Addin>(
        runtime: &'static MacroRuntime<A>,
        addin_id: &'static str,
        _display_name: &'static str,
        default_category: &'static str,
        version: &'static str,
        target: &'static str,
        module_anchor: *const (),
    ) -> i32 {
        #[cfg(feature = "lifecycle-diagnostics")]
        let diagnostic =
            crate::lifecycle_diagnostics::Boundary::enter("xlAutoOpen", runtime.runtime().phase());
        let newly_acquired = match runtime.runtime().ensure_module_residency(module_anchor) {
            Ok(newly_acquired) => newly_acquired,
            Err(error) => crate::boundary::fail_stop_module_residency(&error),
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
        let result =
            host_auto_open::<A>(runtime.runtime(), &parsed_id, version, target, &descriptors);
        if result == 0
            && newly_acquired
            && runtime.runtime().phase() != crate::lifecycle::LifecyclePhase::Quarantined
        {
            release_open_residency_after_failure(runtime, true);
        }
        #[cfg(feature = "lifecycle-diagnostics")]
        diagnostic.finish(result, runtime.runtime());
        result
    }

    /// Cleans up a deactivated add-in and follows its physical-unload contract.
    ///
    /// # Safety
    /// This must be called from Excel's `xlAutoClose` entrypoint on its lifecycle
    /// thread, while Excel permits the lifecycle callbacks performed here.
    /// It must not be used to renew callback authority inside another invocation.
    #[doc(hidden)]
    #[allow(
        unsafe_code,
        reason = "Excel lifecycle entrypoint grants callback authority"
    )]
    pub unsafe fn auto_close_generated_addin<A: Addin>(runtime: &'static MacroRuntime<A>) -> i32 {
        #[cfg(feature = "lifecycle-diagnostics")]
        let diagnostic =
            crate::lifecycle_diagnostics::Boundary::enter("xlAutoClose", runtime.runtime().phase());
        let result = host_auto_close::<A>(runtime.runtime());
        #[cfg(feature = "lifecycle-diagnostics")]
        diagnostic.finish(result, runtime.runtime());
        result
    }

    /// Performs explicit terminal removal and unregisters all functions.
    ///
    /// # Safety
    /// This must be called from Excel's `xlAutoRemove` entrypoint on its lifecycle
    /// thread, while Excel permits the lifecycle callbacks performed here.
    /// It must not be used to renew callback authority inside another invocation.
    #[doc(hidden)]
    #[allow(
        unsafe_code,
        reason = "Excel lifecycle entrypoint grants callback authority"
    )]
    pub unsafe fn auto_remove_generated_addin<A: Addin>(runtime: &'static MacroRuntime<A>) -> i32 {
        #[cfg(feature = "lifecycle-diagnostics")]
        let diagnostic = crate::lifecycle_diagnostics::Boundary::enter(
            "xlAutoRemove",
            runtime.runtime().phase(),
        );
        let result = host_auto_remove::<A>(runtime.runtime());
        #[cfg(feature = "lifecycle-diagnostics")]
        diagnostic.finish(result, runtime.runtime());
        result
    }

    /// Releases a return value pointer returned to Excel.
    #[doc(hidden)]
    #[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
    pub unsafe fn free_generated_return(pointer: *mut xlfn_sys::XLOPER12) {
        // SAFETY: pointer is a live return block passed back by Excel to xlAutoFree12.
        let free_operation = unsafe { free_return_boundary(pointer) };
        drop(free_operation);
    }

    /// Supplies immutable metadata in every add-in lifecycle phase.
    ///
    /// # Safety
    /// `action` must be the live XLOPER12 supplied by Excel. Excel may only read
    /// the returned static value while the XLL is loaded, and must not pass it
    /// to xlAutoFree12 because its free bits are unset.
    #[doc(hidden)]
    #[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
    pub unsafe fn addin_manager_info(
        metadata: &'static AddinMetadata,
        action: *mut xlfn_sys::XLOPER12,
    ) -> *mut xlfn_sys::XLOPER12 {
        crate::return_abi::metadata::metadata_boundary(|| {
            // SAFETY: action is the live XLOPER12 supplied by Excel.
            unsafe { metadata.query(action) }
        })
    }

    /// Calculation call frame for converted arguments and identity recording.
    #[doc(hidden)]
    pub struct CallFrame<'call, M: InputMode> {
        arguments: ArgumentContext<'call, M>,
        callbacks: &'call crate::host_callback::HostCallbackSession,
        diagnostics: &'call crate::diagnostics::DiagnosticHistory,
    }

    impl<'call, M: InputMode> CallFrame<'call, M> {
        #[doc(hidden)]
        pub(crate) fn new<A: Addin>(
            call: &'call crate::runtime::CallGuard<'_, A>,
            scope: &'call CallScope<'call>,
            callbacks: &'call crate::host_callback::HostCallbackSession,
            argument_count: usize,
        ) -> Self {
            Self {
                arguments: ArgumentContext::new(call, scope, argument_count),
                callbacks,
                diagnostics: call.diagnostics(),
            }
        }

        #[cfg(feature = "rtd")]
        pub(crate) fn rtd_access(&self) -> crate::rtd::RtdGenerationAccess<'call> {
            self.arguments.rtd_access()
        }

        #[doc(hidden)]
        pub fn return_context(
            &mut self,
            udf_id: &'static str,
        ) -> XllResult<ReturnContext<'call, 'call>> {
            let inputs = self.arguments.finish()?;
            #[cfg(feature = "handles")]
            {
                let handles = self.arguments.take_handle_access();
                Ok(ReturnContext::for_frame(
                    handles,
                    udf_id,
                    inputs,
                    self.callbacks,
                ))
            }
            #[cfg(not(feature = "handles"))]
            {
                let _ = (udf_id, inputs);
                Ok(ReturnContext::new())
            }
        }

        #[doc(hidden)]
        pub fn prepare_default<T: ExcelParameter<'call, M>>(
            &mut self,
            index: usize,
            name: &'static str,
            value: T,
        ) -> XllResult<PreparedArgument<T, T::Prepared>> {
            self.arguments.record_decoded(index, name, &value)?;
            Ok(PreparedArgument::Ready(value))
        }

        #[doc(hidden)]
        pub fn prepare_native_f64(
            &mut self,
            index: usize,
            name: &'static str,
            raw: f64,
        ) -> XllResult<PreparedArgument<f64, ()>> {
            self.native_f64(index, name, raw)
                .map(PreparedArgument::Ready)
        }

        #[doc(hidden)]
        pub fn prepare_native_bool(
            &mut self,
            index: usize,
            name: &'static str,
            raw: i16,
        ) -> XllResult<PreparedArgument<bool, ()>> {
            self.native_bool(index, name, raw)
                .map(PreparedArgument::Ready)
        }

        #[doc(hidden)]
        pub fn prepare_native_i32(
            &mut self,
            index: usize,
            name: &'static str,
            raw: i32,
        ) -> XllResult<PreparedArgument<i32, ()>> {
            self.native_i32(index, name, raw)
                .map(PreparedArgument::Ready)
        }

        /// Validates and records a native string before a possible cache hit.
        ///
        /// # Safety
        /// `raw` points to Excel's counted UTF-16 string, live and immutable
        /// for the duration of this call.
        #[doc(hidden)]
        #[allow(unsafe_code, reason = "receives Excel's native counted UTF-16 input")]
        pub unsafe fn prepare_native_string(
            &mut self,
            index: usize,
            name: &'static str,
            raw: *const u16,
        ) -> XllResult<PreparedArgument<String, crate::value::PreparedString<'call>>> {
            // SAFETY: the generated wrapper forwards Excel's live string argument.
            unsafe { self.arguments.prepare_native_string(index, name, raw) }
        }

        /// Validates a native string and prepares its borrowed Rust value.
        /// Formula inputs decode into call scratch storage only on materialization.
        ///
        /// # Safety
        /// `raw` points to Excel's counted UTF-16 string, live and immutable
        /// for the duration of this call.
        #[doc(hidden)]
        #[allow(unsafe_code, reason = "receives Excel's native counted UTF-16 input")]
        pub unsafe fn prepare_native_str(
            &mut self,
            index: usize,
            name: &'static str,
            raw: *const u16,
        ) -> XllResult<PreparedArgument<&'call str, crate::value::PreparedBorrowedString<'call>>>
        {
            // SAFETY: the generated wrapper forwards Excel's live string argument.
            unsafe { self.arguments.prepare_native_str(index, name, raw) }
        }

        #[doc(hidden)]
        pub fn native_f64(&mut self, index: usize, name: &'static str, raw: f64) -> XllResult<f64> {
            self.arguments.native_f64(index, name, raw)
        }

        #[doc(hidden)]
        pub fn native_bool(
            &mut self,
            index: usize,
            name: &'static str,
            raw: i16,
        ) -> XllResult<bool> {
            self.arguments.native_bool(index, name, raw)
        }

        #[doc(hidden)]
        pub fn native_i32(&mut self, index: usize, name: &'static str, raw: i32) -> XllResult<i32> {
            self.arguments.native_i32(index, name, raw)
        }

        /// Copies Excel's native string into owned data before an async task.
        ///
        /// # Safety
        /// `raw` points to Excel's counted UTF-16 string, live and immutable
        /// until this method returns. No Excel memory is retained in the result.
        #[doc(hidden)]
        #[allow(
            unsafe_code,
            reason = "copies Excel's native UTF-16 input for async use"
        )]
        pub unsafe fn native_string(
            &mut self,
            index: usize,
            name: &'static str,
            raw: *const u16,
        ) -> XllResult<String> {
            // SAFETY: Excel's input is live throughout this owned conversion.
            unsafe { self.arguments.native_string(index, name, raw) }
        }

        #[doc(hidden)]
        #[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
        pub unsafe fn prepare_argument<T: ExcelParameter<'call, M>>(
            &mut self,
            index: usize,
            name: &'static str,
            raw: *mut xlfn_sys::XLOPER12,
        ) -> XllResult<PreparedArgument<T, T::Prepared>> {
            // SAFETY: Excel owns raw for the duration of this synchronous call.
            let borrowed =
                unsafe { crate::value::XlValueRef::from_raw(raw) }.map_err(
                    |error| match error {
                        XllError::Input { reason, .. } => XllError::Input {
                            argument: name,
                            reason,
                        },
                        other => other,
                    },
                )?;
            self.arguments.prepare(index, name, borrowed)
        }

        /// Borrows Excel's FP12 argument while retaining input identity when required.
        ///
        /// # Safety
        ///
        /// `raw` is a live, immutable FP12 argument for this Excel invocation.
        #[doc(hidden)]
        #[allow(
            unsafe_code,
            reason = "FP12 input is borrowed at the generated ABI boundary"
        )]
        pub unsafe fn prepare_numeric_matrix(
            &mut self,
            index: usize,
            name: &'static str,
            raw: *const xlfn_sys::FP12,
        ) -> XllResult<PreparedArgument<crate::value::NumericMatrixRef<'call>, ()>> {
            // SAFETY: the generated wrapper forwards Excel's live FP12 argument.
            let value = unsafe { crate::value::NumericMatrixRef::from_raw(raw, name) }?;
            self.arguments.record_numeric_matrix(index, name, value)?;
            Ok(PreparedArgument::Ready(value))
        }

        #[doc(hidden)]
        pub fn default_argument<T>(
            &mut self,
            index: usize,
            name: &'static str,
            value: T,
        ) -> XllResult<T>
        where
            T: ExcelParameter<'call, M>,
        {
            self.arguments.record_decoded(index, name, &value)?;
            Ok(value)
        }

        #[doc(hidden)]
        #[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
        pub unsafe fn convert_argument<T>(
            &mut self,
            index: usize,
            name: &'static str,
            raw: *mut xlfn_sys::XLOPER12,
        ) -> XllResult<T>
        where
            T: ExcelParameter<'call, M>,
        {
            // SAFETY: raw is supplied by Excel for this call.
            unsafe {
                argument_from_raw_with_arguments::<M, T>(&mut self.arguments, index, name, raw)
            }
        }

        /// Decodes an async `HandleLease` argument while the Excel call is
        /// still admitted. The returned pending pin is consumed only by the
        /// generated scoped-task builder.
        #[cfg(all(feature = "async", feature = "handles"))]
        #[doc(hidden)]
        #[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
        pub unsafe fn convert_handle_pending<A, T>(
            &mut self,
            index: usize,
            name: &'static str,
            raw: *mut xlfn_sys::XLOPER12,
            lease: &crate::generation::ExecutionLease<A>,
        ) -> XllResult<crate::handle::PendingHandleLease<T>>
        where
            A: Addin,
            T: crate::handle::ExcelHandleObject,
        {
            // SAFETY: raw is supplied by Excel for this call.
            let borrowed =
                unsafe { crate::value::XlValueRef::from_raw(raw) }.map_err(
                    |error| match error {
                        XllError::Input { reason, .. } => XllError::Input {
                            argument: name,
                            reason,
                        },
                        other => other,
                    },
                )?;
            self.arguments
                .decode_pending_handle(index, name, borrowed, lease.generation())
        }

        #[doc(hidden)]
        #[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
        pub unsafe fn convert_reference<T>(
            &mut self,
            name: &'static str,
            raw: *mut xlfn_sys::XLOPER12,
        ) -> XllResult<T>
        where
            T: FromExcelReference<'call>,
        {
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
    pub unsafe fn convert_argument<'call, R, T>(
        frame: &mut CallFrame<'call, R::InputMode>,
        index: usize,
        name: &'static str,
        raw: *mut xlfn_sys::XLOPER12,
    ) -> XllResult<T>
    where
        R: ExcelReturn,
        T: ExcelParameter<'call, R::InputMode>,
    {
        // SAFETY: caller guarantees raw is live for this call.
        unsafe { frame.convert_argument(index, name, raw) }
    }

    /// Binds one decode-time pending handle pin to the generated async task's
    /// narrow lifetime brand.
    #[cfg(all(feature = "async", feature = "handles"))]
    #[doc(hidden)]
    pub fn bind_handle_lease<'generation, T>(
        pending: crate::handle::PendingHandleLease<T>,
        scope: AsyncTaskScope<'generation>,
    ) -> crate::handle::HandleLease<'generation, T>
    where
        T: crate::handle::ExcelHandleObject,
    {
        pending.bind(scope)
    }

    /// Helper free function to convert a reference argument from a raw pointer using the active call frame.
    #[doc(hidden)]
    #[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
    pub unsafe fn convert_reference<'call, M: InputMode, T>(
        frame: &mut CallFrame<'call, M>,
        name: &'static str,
        raw: *mut xlfn_sys::XLOPER12,
    ) -> XllResult<T>
    where
        T: FromExcelReference<'call>,
    {
        // SAFETY: caller guarantees raw is live for this call.
        unsafe { frame.convert_reference(name, raw) }
    }

    /// Helper free function to query cell presence for default/missing policies.
    #[doc(hidden)]
    #[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
    pub unsafe fn argument_presence<M: InputMode>(
        frame: &CallFrame<'_, M>,
        name: &'static str,
        raw: *mut xlfn_sys::XLOPER12,
    ) -> XllResult<CellPresence> {
        // SAFETY: caller guarantees raw is live for this call.
        unsafe { frame.argument_presence(name, raw) }
    }

    /// Top-level synchronous UDF execution boundary.
    ///
    /// # Safety
    /// This must be called exactly once from an actual Excel UDF entrypoint
    /// with the registered execution mode on Excel's permitted thread. It
    /// creates fresh callback authority and must never be called to restart
    /// an active invocation, including after Abort or Uncalced.
    #[doc(hidden)]
    #[allow(unsafe_code, reason = "Generated Excel entrypoint authority boundary")]
    pub unsafe fn sync_udf<A: Addin, R: ExcelReturn, F>(
        runtime: &'static MacroRuntime<A>,
        udf_id: &'static str,
        excel_name: &'static str,
        argument_count: usize,
        execute: F,
    ) -> *mut xlfn_sys::XLOPER12
    where
        F: for<'call> FnOnce(
            &'call A::SharedState,
            &mut CallFrame<'call, R::InputMode>,
        ) -> XllResult<ExcelOutput>,
    {
        udf_boundary_named(runtime.runtime(), udf_id, excel_name, |_, call| {
            let callbacks = crate::host_callback::HostCallbackSession::new();
            crate::call::with_excel_callback_scope_and_call(
                call,
                &callbacks,
                |call, scope, callbacks| {
                    let mut frame =
                        CallFrame::<R::InputMode>::new(call, scope, callbacks, argument_count);
                    execute(call.state(), &mut frame)
                },
            )
        })
    }

    /// Top-level synchronous FP12 UDF execution boundary.
    ///
    /// # Safety
    /// The caller must satisfy the entrypoint and callback authority contract of [`sync_udf`].
    #[doc(hidden)]
    #[allow(unsafe_code, reason = "Generated Excel entrypoint authority boundary")]
    pub unsafe fn sync_udf_fp12<A: Addin, R: NumericReturn, F>(
        runtime: &'static MacroRuntime<A>,
        udf_id: &'static str,
        excel_name: &'static str,
        argument_count: usize,
        execute: F,
    ) -> *mut xlfn_sys::FP12
    where
        F: for<'call> FnOnce(
            &'call A::SharedState,
            &mut CallFrame<'call, R::InputMode>,
        ) -> XllResult<crate::output::NumericArrayOutput>,
    {
        crate::return_abi::udf_boundary_numeric_named(
            runtime.runtime(),
            udf_id,
            excel_name,
            |_, call| {
                let callbacks = crate::host_callback::HostCallbackSession::new();
                crate::call::with_excel_callback_scope_and_call(
                    call,
                    &callbacks,
                    |call, scope, callbacks| {
                        let mut frame =
                            CallFrame::<R::InputMode>::new(call, scope, callbacks, argument_count);
                        execute(call.state(), &mut frame)
                    },
                )
            },
        )
    }

    /// Top-level asynchronous UDF execution boundary.
    ///
    /// # Safety
    /// Excel must invoke this boundary exactly once in the registered async
    /// execution mode, with a live async handle and live argument pointers.
    /// It grants entrypoint callback authority during input conversion and
    /// must never restart another active invocation after Abort or Uncalced.
    #[cfg(feature = "async")]
    #[doc(hidden)]
    #[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
    pub unsafe fn async_udf<A, R, F, Fut>(
        runtime: &'static MacroRuntime<A>,
        udf_id: &'static str,
        excel_name: &'static str,
        argument_count: usize,
        async_handle: *mut xlfn_sys::XLOPER12,
        execute: F,
    ) where
        A: Addin,
        R: ExcelReturn + Send + 'static,
        F: for<'call> FnOnce(
            &'call crate::runtime::CallGuard<'call, A>,
            crate::generation::ExecutionLease<A>,
            CancellationToken,
            &mut CallFrame<'call, R::InputMode>,
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
                |call, lease, cancellation| {
                    let callbacks = crate::host_callback::HostCallbackSession::new();
                    crate::call::with_excel_callback_scope_and_call(
                        call,
                        &callbacks,
                        |call, scope, callbacks| {
                            let mut frame = CallFrame::<R::InputMode>::new(
                                call,
                                scope,
                                callbacks,
                                argument_count,
                            );
                            let future = execute(call, lease, cancellation, &mut frame)?;
                            frame.arguments.finish()?;
                            Ok(future)
                        },
                    )
                },
            )
        }
    }

    /// Top-level asynchronous UDF execution boundary for generation-scoped
    /// handle leases.
    ///
    /// # Safety
    /// Excel must invoke this boundary exactly once in the registered async
    /// execution mode, with a live async handle and live argument pointers.
    /// It grants entrypoint callback authority during input conversion and
    /// must never restart another active invocation after Abort or Uncalced.
    #[cfg(all(feature = "async", feature = "handles"))]
    #[doc(hidden)]
    #[allow(unsafe_code, reason = "Internal C-ABI raw memory access")]
    pub unsafe fn async_udf_handle<A, R, F, Build>(
        runtime: &'static MacroRuntime<A>,
        udf_id: &'static str,
        excel_name: &'static str,
        argument_count: usize,
        async_handle: *mut xlfn_sys::XLOPER12,
        execute: F,
    ) where
        A: Addin,
        R: ExcelReturn + Send + 'static,
        F: for<'call> FnOnce(
            &'call crate::runtime::CallGuard<'call, A>,
            crate::generation::ExecutionLease<A>,
            CancellationToken,
            &mut CallFrame<'call, R::InputMode>,
        ) -> XllResult<Build>,
        Build: HandleScopedBuilder<R> + Send + 'static,
    {
        // SAFETY: async_handle is supplied by Excel for this call.
        unsafe {
            crate::async_udf::async_udf_boundary_named_handle(
                runtime.runtime(),
                udf_id,
                excel_name,
                async_handle,
                |call, lease, cancellation| {
                    let callbacks = crate::host_callback::HostCallbackSession::new();
                    crate::call::with_excel_callback_scope_and_call(
                        call,
                        &callbacks,
                        |call, scope, callbacks| {
                            let mut frame = CallFrame::<R::InputMode>::new(
                                call,
                                scope,
                                callbacks,
                                argument_count,
                            );
                            let build = execute(call, lease, cancellation, &mut frame)?;
                            frame.arguments.finish()?;
                            Ok(build)
                        },
                    )
                },
            )
        }
    }

    /// Cancels calculation in the async runtime.
    #[cfg(feature = "async")]
    #[doc(hidden)]
    pub fn cancel_async_calculation<A: Addin>(runtime: &'static MacroRuntime<A>) {
        // Excel can retain this export for the DLL lifetime. The common
        // boundary admits it only while a generation is published and keeps
        // that generation alive until cancellation returns.
        ffi_boundary_void(runtime.runtime(), || {
            crate::async_udf::cancel_async_calculation(runtime.runtime());
        });
    }

    /// Advances diagnostic calculation identity and async completion state.
    #[doc(hidden)]
    pub fn end_calculation<A: Addin>(runtime: &'static MacroRuntime<A>) {
        // Inactive generations must not advance calculation identity merely
        // because Excel still holds the module's event registration.
        ffi_boundary_void(runtime.runtime(), || {
            runtime.runtime().finish_calculation();
        });
    }

    /// Publishes a new handle instance inside a return context.
    #[cfg(feature = "handles")]
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
            .and_then(ExcelOutput::scalar)
    }

    // External compile-fail fixtures build their dependency without cfg(test).
    #[cfg(all(feature = "handles", feature = "bench-internals"))]
    #[doc(hidden)]
    pub mod handle_test {
        use crate::call::CallScope;
        use crate::error::XllResult;
        use crate::handle::{ExcelHandleObject, Handle};

        pub struct HandleRegistry(crate::handle::HandleRegistry);

        impl HandleRegistry {
            pub fn new(capacity: usize) -> Self {
                Self(
                    crate::handle::HandleRegistry::try_new(
                        u32::try_from(capacity).expect("handle capacity fits u32"),
                    )
                    .expect("valid registry"),
                )
            }

            pub fn insert_object<T: ExcelHandleObject>(&self, value: T) -> XllResult<String> {
                let pending = self.0.new_object(value)?;
                let (token, ..) = self.0.publish_pending::<T>(pending)?;
                Ok(token)
            }

            pub fn lookup_handle<'call, T: ExcelHandleObject>(
                &'call self,
                scope: &'call CallScope<'call>,
                token: &str,
            ) -> XllResult<Handle<'call, T>> {
                self.0.lookup_handle::<T>(scope, token)
            }
        }
    }

    #[cfg(test)]
    mod native_input_tests {
        use super::*;

        #[test]
        #[allow(
            unsafe_code,
            reason = "tests native admission with an owned counted string buffer"
        )]
        fn miri_native_async_string_owns_bytes_after_excel_input_is_released() {
            fn convert<M: InputMode>() -> String {
                let text = "価格\0💡";
                let state = (
                    crate::utf16::encode_counted(text, "label", 32_767).unwrap(),
                    crate::host_callback::HostCallbackSession::new(),
                    crate::diagnostics::DiagnosticHistory::new(),
                );
                with_excel_call_scope_and_state(&state, |state, scope| {
                    let mut frame = CallFrame::<M> {
                        arguments: ArgumentContext::from_scope(scope, 1),
                        callbacks: &state.1,
                        diagnostics: &state.2,
                    };
                    // SAFETY: state owns the complete immutable counted input
                    // while admission copies it into the returned String.
                    let value =
                        unsafe { frame.native_string(0, "label", state.0.as_ptr()) }.unwrap();
                    frame.arguments.finish().unwrap();
                    value
                })
            }
            fn assert_async_owned<T: Send + 'static>(_: &T) {}
            for owned in [convert::<PlainInputMode>(), convert::<FormulaInputMode>()] {
                assert_eq!(owned, "価格\0💡");
                assert_async_owned(&owned);
            }
        }
    }

    #[cfg(test)]
    mod calculation_event_tests {
        use super::{MacroRuntime, end_calculation};
        use crate::boundary::host::host_auto_close;
        use crate::lifecycle::LifecyclePhase;

        struct CallbackAddin;

        impl crate::Addin for CallbackAddin {
            type SharedState = &'static MacroRuntime<Self>;
            type LifecycleState = ();
            type Error = crate::XllError;
            type Layers = ();
            #[cfg(all(feature = "async", not(feature = "async-builtin")))]
            type AsyncExecutor = crate::NoAsyncExecutor;
            #[cfg(feature = "async-builtin")]
            type AsyncExecutor = crate::BuiltinAsyncExecutor;

            fn open(_: &crate::OpenContext) -> crate::OpenResult<Self> {
                unreachable!("the test publishes its runtime reference directly")
            }

            fn quiesce(shared: &mut Self::SharedState, _: &mut ()) -> crate::XllResult<()> {
                assert_eq!(shared.runtime().phase(), LifecyclePhase::Closing);
                assert_inactive_events(shared);
                Ok(())
            }
        }

        fn assert_inactive_events(runtime: &'static MacroRuntime<CallbackAddin>) {
            let calculation = runtime.runtime().calculation_id();
            let phase = runtime.runtime().phase();
            end_calculation(runtime);
            #[cfg(feature = "async")]
            super::cancel_async_calculation(runtime);
            assert_eq!(runtime.runtime().calculation_id(), calculation);
            assert_eq!(runtime.runtime().phase(), phase);
        }

        #[test]
        fn resident_calculation_callbacks_only_reach_the_published_generation() {
            let runtime = Box::leak(Box::new(MacroRuntime::<CallbackAddin>::new()));
            let _fixture = crate::runtime::StaticTestRuntime::from_static(runtime.runtime());
            assert_inactive_events(runtime);
            assert!(!runtime.runtime().has_current_generation());

            for _ in 0..2 {
                let opening = runtime.runtime().begin_open().unwrap();
                #[cfg(all(feature = "async", not(feature = "async-builtin")))]
                runtime
                    .runtime()
                    .start_async(crate::NoAsyncExecutor, crate::AsyncTaskLimit::DEFAULT)
                    .unwrap();
                #[cfg(feature = "async-builtin")]
                runtime
                    .runtime()
                    .start_async(
                        crate::BuiltinAsyncExecutor::new(
                            crate::BuiltinExecutorConfig::new()
                                .with_poller_count(crate::AsyncPollerCount::new(1).unwrap()),
                        ),
                        crate::AsyncTaskLimit::DEFAULT,
                    )
                    .unwrap();
                #[cfg(feature = "async-builtin")]
                let cancellation = {
                    let (source, token) = crate::cancellation::CancellationSource::new(
                        crate::cancellation::CancellationGuarantee::CalculationScoped,
                    );
                    let asynchronous = runtime.runtime().async_runtime();
                    asynchronous
                        .submit(asynchronous.current_epoch(), std::future::pending(), source)
                        .unwrap();
                    token
                };
                assert_inactive_events(runtime);
                #[cfg(feature = "async-builtin")]
                assert!(!cancellation.is_cancelled());
                let mut opening = runtime.runtime().publish(opening, &*runtime, ());
                assert_inactive_events(runtime);
                #[cfg(feature = "async-builtin")]
                assert!(!cancellation.is_cancelled());
                runtime
                    .runtime()
                    .finish_open(&mut opening, Vec::new())
                    .unwrap();
                drop(opening);

                #[cfg(feature = "async-builtin")]
                {
                    super::cancel_async_calculation(runtime);
                    assert!(cancellation.is_cancelled());
                }

                let calculation = runtime.runtime().calculation_id();
                end_calculation(runtime);
                assert_eq!(
                    runtime.runtime().calculation_id().get(),
                    calculation.get() + 1
                );

                assert_eq!(host_auto_close(runtime.runtime()), 1);
                assert_eq!(runtime.runtime().phase(), LifecyclePhase::Closed);
                assert!(!runtime.runtime().has_current_generation());
                assert_inactive_events(runtime);
            }

            runtime.runtime().quarantine_runtime();
            assert_eq!(runtime.runtime().phase(), LifecyclePhase::Quarantined);
            assert_inactive_events(runtime);
        }
    }

    #[cfg(test)]
    #[allow(
        unsafe_code,
        reason = "Tests opt into the audited physical-unload contract"
    )]
    mod open_failure_tests {
        use super::{MacroRuntime, release_open_residency_after_failure};
        use crate::XllError;
        use crate::addin::{Addin, BuildInfo, OpenContext, Opened, PhysicallyUnloadableAddin};
        use crate::boundary::host::{host_auto_close, host_auto_open, host_auto_remove};
        use crate::diagnostics::AddinId;
        use crate::lifecycle::LifecyclePhase;
        use crate::runtime::test_support::initialize_addin;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct ErrorBeforeState;

        static OPEN_CALLS: AtomicUsize = AtomicUsize::new(0);
        static QUIESCE_CALLS: AtomicUsize = AtomicUsize::new(0);

        impl Addin for ErrorBeforeState {
            type SharedState = ();
            type LifecycleState = ();
            type Layers = ();
            #[cfg(feature = "async")]
            type AsyncExecutor = crate::NoAsyncExecutor;
            type Error = XllError;

            fn open(_: &OpenContext) -> Result<Opened<()>, XllError> {
                OPEN_CALLS.fetch_add(1, Ordering::Relaxed);
                Err(XllError::Overloaded)
            }

            fn quiesce(_: &mut (), _: &mut ()) -> Result<(), XllError> {
                QUIESCE_CALLS.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
        }

        // SAFETY: This test add-in has no external execution sources. Its
        // failing open exercises the framework's conservative residency rule.
        unsafe impl PhysicallyUnloadableAddin for ErrorBeforeState {}

        #[cfg(any(not(target_os = "windows"), feature = "async", feature = "handles"))]
        struct ErrorAfterEventRegistration;

        #[cfg(any(not(target_os = "windows"), feature = "async", feature = "handles"))]
        impl Addin for ErrorAfterEventRegistration {
            type SharedState = ();
            type LifecycleState = ();
            type Layers = ();
            #[cfg(feature = "async")]
            type AsyncExecutor = crate::NoAsyncExecutor;
            type Error = XllError;

            fn open(_: &OpenContext) -> Result<Opened<()>, XllError> {
                Ok(Opened::new(()))
            }
        }

        // SAFETY: This fixture owns no external execution sources. Native
        // event residency remains a framework obligation despite this opt-in.
        #[cfg(any(not(target_os = "windows"), feature = "async", feature = "handles"))]
        unsafe impl PhysicallyUnloadableAddin for ErrorAfterEventRegistration {}

        #[inline(never)]
        fn residency_anchor() {}

        #[test]
        fn application_open_error_keeps_residency_and_rejects_reopen() {
            for previously_removed in [false, true] {
                let macro_runtime = MacroRuntime::<ErrorBeforeState>::new_with_physical_unload();
                let runtime = macro_runtime.runtime();
                let newly_acquired = runtime
                    .ensure_module_residency(residency_anchor as *const ())
                    .unwrap();
                assert!(newly_acquired);

                if previously_removed {
                    let opening = runtime.begin_open().unwrap();
                    let mut opening = runtime.publish(opening, (), ());
                    runtime.finish_open(&mut opening, Vec::new()).unwrap();
                    assert_eq!(host_auto_remove(runtime), 1);
                }

                let opens_before = OPEN_CALLS.load(Ordering::Relaxed);
                let quiesces_before = QUIESCE_CALLS.load(Ordering::Relaxed);
                let transaction = runtime
                    .begin_open()
                    .unwrap()
                    .attach_host(crate::host_callback::HostCallbackSession::new());
                let lifecycle = runtime.bind_addin_lifecycle().unwrap();
                let context = OpenContext::new(
                    std::path::PathBuf::from("test.xll"),
                    BuildInfo::new(AddinId::parse("test").unwrap(), "0", "test"),
                    runtime.protocol_generation().unwrap(),
                );
                let failure = match initialize_addin::<ErrorBeforeState>(context, transaction) {
                    Ok(_) => panic!("application initialization must fail"),
                    Err(failure) => failure,
                };
                assert!(matches!(
                    failure.recover(runtime, &lifecycle),
                    XllError::Overloaded
                ));
                assert_eq!(runtime.phase(), LifecyclePhase::Quarantined);
                assert_eq!(QUIESCE_CALLS.load(Ordering::Relaxed), quiesces_before);

                // Exercise both the failed-open release path and later host
                // removal/close hints. Neither may release unproven residency.
                release_open_residency_after_failure(&macro_runtime, newly_acquired);
                assert!(runtime.module_residency_held());
                assert_eq!(host_auto_remove(runtime), 1);
                assert_eq!(host_auto_close(runtime), 1);
                assert!(runtime.module_residency_held());
                assert_eq!(
                    host_auto_open(runtime, &AddinId::parse("test").unwrap(), "0", "test", &[]),
                    0
                );
                assert_eq!(OPEN_CALLS.load(Ordering::Relaxed), opens_before + 1);
            }
        }

        #[test]
        fn failure_before_application_initialization_can_release_residency() {
            let macro_runtime = MacroRuntime::<ErrorBeforeState>::new_with_physical_unload();
            let runtime = macro_runtime.runtime();
            let newly_acquired = runtime
                .ensure_module_residency(residency_anchor as *const ())
                .unwrap();
            let transaction = runtime
                .begin_open()
                .unwrap()
                .attach_host(crate::host_callback::HostCallbackSession::new());
            let lifecycle = runtime.bind_addin_lifecycle().unwrap();
            let failure = transaction.failure(XllError::Overloaded);
            assert!(matches!(
                failure.recover(runtime, &lifecycle),
                XllError::Overloaded
            ));
            assert_eq!(runtime.phase(), LifecyclePhase::Closed);
            release_open_residency_after_failure(&macro_runtime, newly_acquired);
            assert!(!runtime.module_residency_held());
        }

        #[test]
        #[cfg(any(not(target_os = "windows"), feature = "async", feature = "handles"))]
        fn failed_open_keeps_residency_after_native_event_registration() {
            use crate::registration::host::test_support::{CallbackScript, Reply};
            use crate::registration::{RegistrationDescriptor, RegistrationSignature};
            use xlfn_common::{ExecutionKind, FunctionVisibility, ReturnAbi};
            use xlfn_sys::{
                XL_EVENT_REGISTER, XL_GET_NAME, XLERR_NAME, XLF_EVALUATE, XLF_REGISTER, XLOPER12,
                XLOPER12Value, XLRET_FAILED, XLTYPE_STR,
            };

            let macro_runtime =
                MacroRuntime::<ErrorAfterEventRegistration>::new_with_physical_unload();
            let runtime = macro_runtime.runtime();
            let newly_acquired = runtime
                .ensure_module_residency(residency_anchor as *const ())
                .unwrap();
            assert!(newly_acquired);
            let path = if cfg!(target_os = "windows") {
                "C:\\test.xll"
            } else {
                "/test.xll"
            };
            let mut module_name: Vec<u16> = path.encode_utf16().collect();
            module_name.insert(0, u16::try_from(module_name.len()).unwrap());
            let script = CallbackScript::install([
                Reply::success(
                    XL_GET_NAME,
                    XLOPER12 {
                        value: XLOPER12Value {
                            string: module_name.as_mut_ptr(),
                        },
                        xltype: XLTYPE_STR,
                    },
                ),
                Reply::success(XL_EVENT_REGISTER, XLOPER12::integer(17)),
                Reply::success(XLF_EVALUATE, XLOPER12::error(XLERR_NAME)),
                Reply::success(XLF_REGISTER, XLOPER12::nil()).status(XLRET_FAILED),
            ]);
            let descriptor = RegistrationDescriptor {
                export_name: "test_failed_open_export",
                excel_name: "TEST.FAILED.OPEN",
                signature: RegistrationSignature {
                    execution: ExecutionKind::MainThread,
                    return_abi: ReturnAbi::Value12,
                    arguments: &[],
                    volatile: false,
                },
                category: "Test",
                description: "failed open event residency regression",
                help_topic: "",
                visibility: FunctionVisibility::Public,
                arguments: &[],
            };
            assert_eq!(
                host_auto_open(
                    runtime,
                    &AddinId::parse("event-residency").unwrap(),
                    "0",
                    "test",
                    &[descriptor],
                ),
                0
            );
            assert_eq!(runtime.phase(), LifecyclePhase::Closed);
            assert!(!runtime.has_current_generation());
            assert_eq!(runtime.resident_event_count(), 1);

            // This is the generated xlAutoOpen failure epilogue. A fully
            // rolled-back generation does not mean its native export is gone.
            release_open_residency_after_failure(&macro_runtime, newly_acquired);
            assert!(runtime.module_residency_held());
            assert_eq!(host_auto_remove(runtime), 1);
            assert_eq!(host_auto_close(runtime), 1);
            assert!(runtime.module_residency_held());
            assert_eq!(runtime.resident_event_count(), 1);
            script.assert_calls(&[XL_GET_NAME, XL_EVENT_REGISTER, XLF_EVALUATE, XLF_REGISTER]);
        }
    }
}

#[cfg(all(feature = "handles", feature = "bench-internals"))]
#[doc(hidden)]
pub use v1::handle_test;

#[cfg(test)]
mod tests {
    use super::v1;
    use crate::panic_boundary::tests::PanickingPayload;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug, PartialEq, crate::ExcelEnum)]
    #[excel_enum(ascii_case_insensitive)]
    enum Mode {
        #[excel_enum(name = "Fast mode")]
        Fast,
        Slow,
    }

    fn decode_mode(mut utf16: Vec<u16>) -> crate::XllResult<Mode> {
        use crate::value::FromExcel;
        utf16.insert(0, u16::try_from(utf16.len()).unwrap());
        let raw = xlfn_sys::XLOPER12 {
            value: xlfn_sys::XLOPER12Value {
                string: utf16.as_mut_ptr(),
            },
            xltype: xlfn_sys::XLTYPE_STR,
        };
        // The raw root and its counted UTF-16 buffer remain live for decoding.
        let value = crate::value::XlValueRef::from_array_cell(&raw).unwrap();
        Mode::from_excel(value, "mode")
    }

    #[test]
    fn enum_conversion_reports_input_and_choices_and_keeps_matching_semantics() {
        assert_eq!(
            decode_mode("fAsT mOdE".encode_utf16().collect()).unwrap(),
            Mode::Fast
        );
        let error = decode_mode("unknown日本語".encode_utf16().collect()).unwrap_err();
        let crate::XllError::Input { argument, reason } = &error else {
            panic!("enum failures must retain input classification");
        };
        assert_eq!(*argument, "mode");
        assert!(matches!(
            reason,
            crate::error::InputError::UnknownEnum { actual, expected }
                if actual == "unknown日本語" && *expected == "[\"Fast mode\", \"Slow\"]"
        ));
        assert!(error.to_string().contains("unknown日本語"));
        assert_eq!(error.excel_error(), crate::ExcelError::Value);
    }

    #[test]
    fn enum_diagnostics_bound_unicode_prefix_and_still_reject_invalid_utf16() {
        let error = decode_mode("日".repeat(1000).encode_utf16().collect()).unwrap_err();
        let crate::XllError::Input {
            reason: crate::error::InputError::UnknownEnum { actual, .. },
            ..
        } = error
        else {
            panic!("unknown enum diagnostic expected");
        };
        assert!(actual.len() <= 1024 + "…[truncated]".len());
        assert!(actual.ends_with("…[truncated]"));
        assert!(
            actual
                .trim_end_matches("…[truncated]")
                .chars()
                .all(|ch| ch == '日')
        );

        let mut invalid = vec![u16::from(b'x'); 2000];
        invalid.push(0xd800);
        assert!(matches!(
            decode_mode(invalid),
            Err(crate::XllError::Input {
                reason: crate::error::InputError::InvalidUtf16,
                ..
            })
        ));
    }

    #[test]
    fn generated_export_boundaries_contain_guard_drop_panics() {
        struct PanicGuard(Arc<AtomicUsize>);
        impl Drop for PanicGuard {
            fn drop(&mut self) {
                std::panic::panic_any(PanickingPayload(Arc::clone(&self.0)));
            }
        }

        // These non-unwinding entrypoints exercise the complete closure,
        // including a guard destructor after its normal return expression.
        extern "system" fn value(dropped: &Arc<AtomicUsize>) -> *mut xlfn_sys::XLOPER12 {
            v1::export_value_boundary(|| {
                let _guard = PanicGuard(Arc::clone(dropped));
                std::ptr::null_mut()
            })
        }
        extern "system" fn metadata(dropped: &Arc<AtomicUsize>) -> *mut xlfn_sys::XLOPER12 {
            v1::export_metadata_boundary(|| {
                let _guard = PanicGuard(Arc::clone(dropped));
                std::ptr::null_mut()
            })
        }
        extern "system" fn status(dropped: &Arc<AtomicUsize>) -> i32 {
            v1::export_status_boundary(-1, || {
                let _guard = PanicGuard(Arc::clone(dropped));
                1
            })
        }
        extern "system" fn void(dropped: &Arc<AtomicUsize>) {
            v1::export_void_boundary(|| {
                let _guard = PanicGuard(Arc::clone(dropped));
            });
        }

        let dropped = Arc::new(AtomicUsize::new(0));
        let pointer = value(&dropped);
        assert_eq!(pointer, crate::return_abi::closing_error_pointer());
        assert_eq!(
            metadata(&dropped),
            crate::return_abi::metadata::invalid_action_pointer()
        );
        assert_eq!(status(&dropped), -1);
        void(&dropped);
        assert_eq!(dropped.load(Ordering::Acquire), 0);
    }
}
