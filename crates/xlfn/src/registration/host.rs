//! Registration-specific façade over the typed Excel callback protocol.
//!
//! This module owns the Excel registration ABI: argument encoding, callback
//! invocation, result decoding, and the distinction between an applied,
//! rejected, and indeterminate host mutation.  Transaction policy and
//! recovery journals remain in `registrar.rs` and `recovery.rs`.

use crate::callback_value::ExcelCallbackValue;
use crate::error::{ExcelApiFailure, ExcelApiFunction};
use crate::host_api::{ExcelHost, HostInvocation};
use crate::host_callback::HostCallbackSession;
use crate::return_abi::ExcelCallbackStatus;
use crate::value::ExcelInputCells;
use crate::value::input::sealed::ExcelParameterSealed;
use crate::value::input::{PreparedArgument, PreparedElement, PreparedParameterSequence};
use crate::value::{
    DecodeCallContext, ExcelParameter, FromExcel, InputMode, XlValueRef, XlValueType,
};
use crate::{XllError, XllResult};
use std::path::PathBuf;
use std::ptr::NonNull;

use super::RegistrationId;
use super::preflight::ValidatedRegistration;
use super::schema::FunctionVisibility;
pub(crate) use xlfn_host::registration::{CountedModuleName, RegistrationScratch};
use xlfn_host::registration::{CountedString, RegistrationArgs, TemporaryString};
use xlfn_sys::{
    XL_EVENT_REGISTER, XL_GET_NAME, XLERR_NAME, XLF_EVALUATE, XLF_REGISTER, XLF_SET_NAME,
    XLF_UNREGISTER, XLOPER12,
};

/// The result of a host-side registration mutation.
///
/// `Applied` means the host-side mutation is known to have taken effect.  The
/// returned value remains useful even when releasing Excel's result fails,
/// because the mutation must not be repeated.  `Rejected` means the operation
/// was not accepted as a mutation.  `Indeterminate` means the callback may
/// have changed host state but the result cannot establish what happened.
pub(crate) enum RegistrationMutation<T> {
    Applied {
        value: T,
        cleanup: XllResult<()>,
    },
    Rejected {
        error: XllError,
    },
    Indeterminate {
        status: ExcelCallbackStatus,
        error: XllError,
    },
}

/// Registration operations available to the transaction layer.
#[derive(Clone, Copy)]
pub(crate) struct RegistrationHost<'call> {
    excel: ExcelHost<'call>,
}

impl<'call> RegistrationHost<'call> {
    pub(crate) const fn new(callbacks: &'call HostCallbackSession) -> Self {
        Self {
            excel: ExcelHost::new(callbacks),
        }
    }

    pub(crate) fn permits_callbacks(&self) -> bool {
        self.excel.permits_callbacks()
    }

    pub(crate) fn terminal_status(&self) -> Option<ExcelCallbackStatus> {
        self.excel.terminal_status()
    }

    pub(crate) fn module_name(&self) -> XllResult<ModuleName> {
        self.excel
            .invoke(XL_GET_NAME, ExcelApiFunction::GetName, &[], |result| {
                decode_module_name(result.borrow()?)
            })
    }

    /// Encodes and invokes `xlfRegister` without exposing its ABI to the
    /// registration transaction layer.
    pub(super) fn register(
        &self,
        module: &CountedModuleName,
        registration: &ValidatedRegistration,
        scratch: &mut RegistrationScratch,
    ) -> RegistrationMutation<RegistrationId> {
        let descriptor = registration.descriptor();
        let args = RegistrationArgs {
            export_name: descriptor.export_name,
            excel_name: descriptor.excel_name,
            signature: registration.signature_text(),
            argument_names: registration.argument_names(),
            macro_type: macro_type(descriptor.visibility),
            category: descriptor.category,
            help_topic: descriptor.help_topic,
            function_help: descriptor.description,
            argument_help: registration
                .argument_help()
                .iter()
                .map(|argument| argument.description),
        };
        match scratch.prepare(args, encode_registration_into) {
            Ok(prepared) => prepared.with_arguments(module, |pointers| {
                // SAFETY: the prepared frame owns and pins each operand through
                // the synchronous invocation and typed result release.
                let invocation = unsafe {
                    invoke_operation_with_scratch(
                        self.excel,
                        Register {
                            excel_name: descriptor.excel_name,
                        },
                        pointers,
                    )
                };
                mutation_from_invocation(invocation)
            }),
            Err(error) => RegistrationMutation::Rejected { error },
        }
    }

    pub(crate) fn register_event(
        &self,
        procedure: &'static str,
        event: i32,
    ) -> RegistrationMutation<i32> {
        let mut procedure_value = match temporary_string(procedure) {
            Ok(value) => value,
            Err(error) => return RegistrationMutation::Rejected { error },
        };
        let mut event_value = XLOPER12::integer(event);
        let arguments = [
            procedure_value.pointer(),
            NonNull::from_mut(&mut event_value),
        ];
        mutation_from_invocation(invoke_operation(self.excel, RegisterEvent, &arguments))
    }

    pub(crate) fn registration_id(
        &self,
        excel_name: &'static str,
    ) -> XllResult<Option<RegistrationId>> {
        let mut name = temporary_string(excel_name)?;
        let arguments = [name.pointer()];
        self.excel.invoke(
            XLF_EVALUATE,
            ExcelApiFunction::Evaluate,
            &arguments,
            |result| decode_registration_id_result(result, excel_name),
        )
    }

    /// Probes the hidden DLL name namespace that xlfRegister writes. A name
    /// can already be defined there with any worksheet value, independent of
    /// the narrower numeric registration-ID contract used during recovery.
    pub(super) fn is_name_bound(&self, excel_name: &'static str) -> XllResult<bool> {
        let mut name = temporary_string(excel_name)?;
        let arguments = [name.pointer()];
        self.excel.invoke(
            XLF_EVALUATE,
            ExcelApiFunction::Evaluate,
            &arguments,
            |result| match result.value_type()? {
                XlValueType::Error => Ok(error_code(result)? != XLERR_NAME),
                _ => Ok(true),
            },
        )
    }

    pub(crate) fn unregister_registration(
        &self,
        registration: RegistrationId,
    ) -> RegistrationMutation<()> {
        let mut id = XLOPER12::number(registration.id);
        let arguments = [NonNull::from_mut(&mut id)];
        mutation_from_invocation(invoke_operation(self.excel, Unregister, &arguments))
    }

    /// Deletes registration metadata only while its current binding is still
    /// owned by one of the registrations awaiting cleanup. An absent name is
    /// already resolved; a changed or unreadable binding is never deleted.
    pub(crate) fn delete_name_if_binding_matches(
        &self,
        excel_name: &'static str,
        expected_ids: impl IntoIterator<Item = f64>,
    ) -> RegistrationMutation<()> {
        match self.registration_id(excel_name) {
            Ok(None) => RegistrationMutation::Applied {
                value: (),
                cleanup: Ok(()),
            },
            Ok(Some(current)) => {
                if expected_ids.into_iter().any(|id| id == current.id) {
                    self.delete_name(excel_name)
                } else {
                    RegistrationMutation::Rejected {
                        error: crate::error::construct::metadata_debt_binding_changed(excel_name),
                    }
                }
            }
            Err(error) => RegistrationMutation::Rejected { error },
        }
    }

    fn delete_name(&self, excel_name: &'static str) -> RegistrationMutation<()> {
        let mut name = match temporary_string(excel_name) {
            Ok(value) => value,
            Err(error) => return RegistrationMutation::Rejected { error },
        };
        let arguments = [name.pointer()];
        mutation_from_invocation(invoke_operation(self.excel, DeleteName, &arguments))
    }
}

#[derive(Clone, Copy)]
enum DecodeFailureDisposition {
    Rejected,
    Indeterminate,
}

/// One operation owns its callback ID, decoder, diagnostic identity and
/// treatment of a successful callback whose result cannot be decoded.
trait RegistrationOperation {
    type Output;
    const FUNCTION_ID: i32;
    const FUNCTION: ExcelApiFunction;
    const DECODE_FAILURE: DecodeFailureDisposition;
    fn decode(self, result: &mut ExcelCallbackValue<'_>) -> XllResult<Self::Output>;
}

struct Register {
    excel_name: &'static str,
}
struct RegisterEvent;
struct Unregister;
struct DeleteName;

impl RegistrationOperation for Register {
    type Output = RegistrationId;
    const FUNCTION_ID: i32 = XLF_REGISTER;
    const FUNCTION: ExcelApiFunction = ExcelApiFunction::Register;
    const DECODE_FAILURE: DecodeFailureDisposition = DecodeFailureDisposition::Indeterminate;
    fn decode(self, result: &mut ExcelCallbackValue<'_>) -> XllResult<Self::Output> {
        decode_registration_id(result, self.excel_name)
    }
}

impl RegistrationOperation for RegisterEvent {
    type Output = i32;
    const FUNCTION_ID: i32 = XL_EVENT_REGISTER;
    const FUNCTION: ExcelApiFunction = ExcelApiFunction::EventRegister;
    const DECODE_FAILURE: DecodeFailureDisposition = DecodeFailureDisposition::Indeterminate;
    fn decode(self, result: &mut ExcelCallbackValue<'_>) -> XllResult<Self::Output> {
        decode_event_registration_id(result)
    }
}

impl RegistrationOperation for Unregister {
    type Output = ();
    const FUNCTION_ID: i32 = XLF_UNREGISTER;
    const FUNCTION: ExcelApiFunction = ExcelApiFunction::Unregister;
    const DECODE_FAILURE: DecodeFailureDisposition = DecodeFailureDisposition::Rejected;
    fn decode(self, result: &mut ExcelCallbackValue<'_>) -> XllResult<()> {
        read_applied_bool(result, Self::FUNCTION)
    }
}

impl RegistrationOperation for DeleteName {
    type Output = ();
    const FUNCTION_ID: i32 = XLF_SET_NAME;
    const FUNCTION: ExcelApiFunction = ExcelApiFunction::SetName;
    const DECODE_FAILURE: DecodeFailureDisposition = DecodeFailureDisposition::Rejected;
    fn decode(self, result: &mut ExcelCallbackValue<'_>) -> XllResult<()> {
        read_applied_bool(result, Self::FUNCTION)
    }
}

struct RegistrationInvocation<O: RegistrationOperation> {
    evidence: HostInvocation<O::Output>,
    operation: std::marker::PhantomData<fn(O) -> O>,
}

fn invoke_operation<O: RegistrationOperation>(
    host: ExcelHost<'_>,
    operation: O,
    arguments: &[NonNull<XLOPER12>],
) -> RegistrationInvocation<O> {
    RegistrationInvocation {
        evidence: host
            .invoke_protocol(O::FUNCTION_ID, arguments, |result| operation.decode(result)),
        operation: std::marker::PhantomData,
    }
}

/// The pointer array is writable and may be overwritten by Excel. Its
/// operands must remain live, stationary and read-only through release.
unsafe fn invoke_operation_with_scratch<O: RegistrationOperation>(
    host: ExcelHost<'_>,
    operation: O,
    arguments: &mut [*mut XLOPER12],
) -> RegistrationInvocation<O> {
    // SAFETY: forwarded from this operation's caller.
    let evidence = unsafe {
        host.invoke_protocol_with_scratch(O::FUNCTION_ID, arguments, |result| {
            operation.decode(result)
        })
    };
    RegistrationInvocation {
        evidence,
        operation: std::marker::PhantomData,
    }
}

fn mutation_from_invocation<O: RegistrationOperation>(
    invocation: RegistrationInvocation<O>,
) -> RegistrationMutation<O::Output> {
    let function = O::FUNCTION;
    let decode_failure = O::DECODE_FAILURE;
    let invocation = invocation.evidence;
    match invocation {
        HostInvocation::Suppressed { status } => RegistrationMutation::Rejected {
            error: crate::error::construct::excel_api(
                function,
                ExcelApiFailure::Suppressed(status),
            ),
        },
        HostInvocation::Failed { status, cleanup } => {
            let status = status.status();
            if crate::error::callback_status_is_terminal(status) {
                return RegistrationMutation::Indeterminate {
                    status,
                    error: cleanup.err().unwrap_or(crate::error::construct::excel_api(
                        function,
                        ExcelApiFailure::Status(status),
                    )),
                };
            }
            RegistrationMutation::Rejected {
                error: cleanup.err().unwrap_or(crate::error::construct::excel_api(
                    function,
                    ExcelApiFailure::Status(status),
                )),
            }
        }
        HostInvocation::Decoded { decoded, cleanup } => match decoded {
            Ok(value) => RegistrationMutation::Applied { value, cleanup },
            Err(error) => {
                let error = if matches!(decode_failure, DecodeFailureDisposition::Indeterminate) {
                    cleanup.err().unwrap_or(error)
                } else {
                    error
                };
                if matches!(decode_failure, DecodeFailureDisposition::Indeterminate) {
                    RegistrationMutation::Indeterminate {
                        status: ExcelCallbackStatus::Success,
                        error,
                    }
                } else {
                    RegistrationMutation::Rejected { error }
                }
            }
        },
    }
}

fn decode_registration_id(
    result: &mut ExcelCallbackValue<'_>,
    excel_name: &'static str,
) -> XllResult<RegistrationId> {
    if result.value_type()? != XlValueType::Number {
        return Err(unexpected_result(ExcelApiFunction::Register));
    }
    let id = result
        .borrow()
        .and_then(|value| f64::from_excel(value, "registration"))?;
    if !valid_registration_id(id) {
        return Err(unexpected_result(ExcelApiFunction::Register));
    }
    Ok(RegistrationId { id, excel_name })
}

fn decode_registration_id_result(
    result: &mut ExcelCallbackValue<'_>,
    excel_name: &'static str,
) -> XllResult<Option<RegistrationId>> {
    match result.value_type()? {
        XlValueType::Error => {
            if error_code(result)? == XLERR_NAME {
                Ok(None)
            } else {
                Err(unexpected_result(ExcelApiFunction::Evaluate))
            }
        }
        XlValueType::Number => {
            let id = result
                .borrow()
                .and_then(|value| f64::from_excel(value, "registration recovery"))?;
            if !valid_registration_id(id) {
                return Err(unexpected_result(ExcelApiFunction::Evaluate));
            }
            Ok(Some(RegistrationId { id, excel_name }))
        }
        _ => Err(unexpected_result(ExcelApiFunction::Evaluate)),
    }
}

fn error_code(result: &ExcelCallbackValue<'_>) -> XllResult<i32> {
    let raw = result.raw();
    // SAFETY: the caller checked that the result has XLTYPE_ERR.
    Ok(unsafe { raw.value.error })
}

fn read_excel_bool(result: &ExcelCallbackValue<'_>, function: ExcelApiFunction) -> XllResult<bool> {
    if result.value_type()? != XlValueType::Boolean {
        return Err(unexpected_result(function));
    }
    let raw = result.raw();
    // SAFETY: XLTYPE_BOOL selects the boolean union member.
    Ok(unsafe { raw.value.boolean } != 0)
}

fn read_applied_bool(result: &ExcelCallbackValue<'_>, function: ExcelApiFunction) -> XllResult<()> {
    if read_excel_bool(result, function)? {
        Ok(())
    } else {
        Err(unexpected_result(function))
    }
}

fn decode_event_registration_id(result: &ExcelCallbackValue<'_>) -> XllResult<i32> {
    if result.value_type()? != XlValueType::Integer {
        return Err(unexpected_result(ExcelApiFunction::EventRegister));
    }
    let raw = result.raw();
    // SAFETY: XLTYPE_INT selects the integer union member.
    let value = unsafe { raw.value.integer };
    // Although the documented success value is positive, live Excel has
    // returned a high-bit acknowledgement (0x9d380001). Treat registration
    // acknowledgements as opaque nonzero values, retaining all their bits.
    // Rejecting the sign aborts xlAutoOpen before any UDF is registered.
    // This does not qualify the separate, undocumented removal convention.
    if value == 0 {
        return Err(crate::error::construct::excel_api(
            ExcelApiFunction::EventRegister,
            ExcelApiFailure::InvalidRegistrationId(value),
        ));
    }
    Ok(value)
}

fn unexpected_result(function: ExcelApiFunction) -> XllError {
    crate::error::construct::excel_api(function, ExcelApiFailure::UnexpectedResult)
}

pub(crate) fn valid_registration_id(id: f64) -> bool {
    // Excel registration IDs are opaque numeric tokens, not positive counters.
    // Windows Excel can return a negative ID for a successful xlfRegister call.
    id.is_finite() && id != 0.0
}

const fn macro_type(visibility: FunctionVisibility) -> f64 {
    match visibility {
        FunctionVisibility::Public => 1.0,
        FunctionVisibility::Hidden => 0.0,
    }
}

fn encode_registration_into(units: &mut CountedString, text: &str) -> XllResult<()> {
    crate::utf16::encode_counted_into(
        text,
        "registration",
        crate::utf16::EXCEL_STRING_LIMIT,
        units,
    )
}

fn temporary_string(text: &str) -> XllResult<TemporaryString> {
    crate::utf16::encode_counted(text, "registration", crate::utf16::EXCEL_STRING_LIMIT)
        .map(TemporaryString::from_counted)
}

pub(crate) struct ModuleName {
    pub(crate) path: PathBuf,
    pub(crate) units: Vec<u16>,
}

impl<'call, M: InputMode> ExcelParameterSealed<'call, M> for ModuleName {}

impl<'call, M: InputMode> ExcelParameter<'call, M> for ModuleName {
    type Prepared = !;
    fn materialize_prepared(value: !) -> XllResult<Self> {
        match value {}
    }
    type ElementPrepared = Self::Prepared;
    fn materialize_element(value: !) -> XllResult<Self> {
        match value {}
    }
    fn prepare_element(
        value: XlValueRef<'call>,
        argument: &'static str,
        context: &mut DecodeCallContext<'_, 'call>,
        identity: &mut M::Identity,
    ) -> XllResult<PreparedElement<'call, Self, M>> {
        <Self as ExcelParameter<'call, M>>::prepare(value, argument, context, identity)
            .map(PreparedArgument::with_materializer)
    }

    type Elements = PreparedParameterSequence<'call, Self, M>;
    fn prepare_elements(
        cells: ExcelInputCells<'call>,
        context: &mut DecodeCallContext<'_, 'call>,
        identity: &mut M::Identity,
    ) -> XllResult<Self::Elements> {
        cells.retain(|value, argument| {
            <Self as ExcelParameter<'call, M>>::prepare_element(value, argument, context, identity)
        })
    }
    fn materialize_elements(elements: Self::Elements) -> XllResult<Vec<Self>> {
        elements.materialize()
    }

    fn decode(
        value: XlValueRef<'call>,
        argument: &'static str,
        _: &mut DecodeCallContext<'_, '_>,
        _: &mut M::Identity,
    ) -> XllResult<Self> {
        Self::from_value(value, argument)
    }

    fn encode_decoded(&self, _: &mut M::Identity) {}
}

impl ModuleName {
    fn from_value(value: XlValueRef<'_>, argument: &'static str) -> XllResult<Self> {
        let units = value.utf16(argument)?.to_vec();
        #[cfg(target_os = "windows")]
        let path = {
            use std::ffi::OsString;
            use std::os::windows::ffi::OsStringExt;
            PathBuf::from(OsString::from_wide(&units))
        };
        #[cfg(not(target_os = "windows"))]
        let path = PathBuf::from(
            String::from_utf16(&units)
                .map_err(|_| XllError::input(argument, crate::error::InputError::InvalidUtf16))?,
        );
        Ok(Self { path, units })
    }
}

fn decode_module_name<'call>(value: XlValueRef<'call>) -> XllResult<ModuleName> {
    ModuleName::from_value(value, "module")
}

// Core callback-session tests need this fixture even without async or handles.
#[cfg(test)]
pub(crate) mod test_support {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use xlfn_sys::{
        XL_FREE, XLF_REGISTER, XLOPER12, XLRET_FAILED, XLRET_SUCCESS, XLTYPE_NUM, XLTYPE_STR,
    };

    #[derive(Clone, Debug, PartialEq)]
    pub(crate) enum RegisterArgument {
        Text(String),
        Number(f64),
        UnexpectedType(u32),
    }

    type RegisterHook = Box<dyn FnOnce(&[*mut XLOPER12])>;

    pub(crate) unsafe fn read_registration_arguments(
        arguments: &[*mut XLOPER12],
    ) -> Vec<RegisterArgument> {
        arguments
            .iter()
            .map(|argument| {
                // SAFETY: the caller retains every operand and its payload.
                let argument = unsafe { &**argument };
                match argument.xltype {
                    XLTYPE_STR => {
                        // SAFETY: the active string is a live counted buffer.
                        let text = unsafe { argument.value.string };
                        // SAFETY: the count prefix describes the live payload.
                        let count = usize::from(unsafe { *text });
                        // SAFETY: the live counted buffer includes its prefix.
                        let payload = unsafe { text.add(1) };
                        // SAFETY: count initialized units follow the prefix.
                        let units = unsafe { std::slice::from_raw_parts(payload, count) };
                        RegisterArgument::Text(String::from_utf16_lossy(units))
                    }
                    // SAFETY: the discriminant selects the numeric member.
                    XLTYPE_NUM => RegisterArgument::Number(unsafe { argument.value.number }),
                    other => RegisterArgument::UnexpectedType(other),
                }
            })
            .collect()
    }

    pub(crate) struct Reply {
        function: i32,
        value: XLOPER12,
        status: i32,
        release_status: i32,
    }

    impl Reply {
        pub(crate) fn success(function: i32, value: XLOPER12) -> Self {
            Self {
                function,
                value,
                status: XLRET_SUCCESS,
                release_status: XLRET_SUCCESS,
            }
        }

        pub(crate) fn status(mut self, status: i32) -> Self {
            self.status = status;
            self
        }

        pub(crate) fn release_status(mut self, status: i32) -> Self {
            self.release_status = status;
            self
        }
    }

    #[derive(Default)]
    struct ScriptState {
        replies: VecDeque<Reply>,
        calls: Vec<i32>,
        release_statuses: Vec<i32>,
        unexpected_calls: usize,
        registration_arguments: Vec<RegisterArgument>,
        registration_arguments_at_release: Vec<RegisterArgument>,
        registration_pointers: Vec<*mut XLOPER12>,
        registration_hook: Option<RegisterHook>,
        overwrite_registration_array: bool,
        call_hook: Option<(i32, Box<dyn FnOnce()>)>,
        argument_hooks: VecDeque<(i32, RegisterHook)>,
    }

    thread_local! {
        static SCRIPT: RefCell<ScriptState> = RefCell::new(ScriptState::default());
    }

    pub(crate) struct CallbackScript {
        _guard: crate::test_callback::CallbackTestGuard,
    }

    impl CallbackScript {
        pub(crate) fn install(replies: impl IntoIterator<Item = Reply>) -> Self {
            let guard = crate::test_callback::lock();
            crate::module_runtime::reset_callbacks_for_test();
            SCRIPT.with_borrow_mut(|script| {
                *script = ScriptState {
                    replies: replies.into_iter().collect(),
                    ..ScriptState::default()
                };
            });
            // SAFETY: the callback has Excel's exact ABI and remains live for
            // the test process. The shared test guard excludes other scripts.
            unsafe {
                xlfn_sys::install_callback_for_abi_probe(
                    callback as *const () as *mut std::ffi::c_void,
                );
            }
            Self { _guard: guard }
        }

        pub(crate) fn assert_calls(&self, expected: &[i32]) {
            SCRIPT.with_borrow(|script| {
                let calls = script
                    .calls
                    .iter()
                    .copied()
                    .filter(|function| *function != XL_FREE)
                    .collect::<Vec<_>>();
                assert_eq!(calls, expected);
                assert!(script.replies.is_empty(), "expected callback was skipped");
                assert!(
                    script.argument_hooks.is_empty(),
                    "expected argument inspection was skipped"
                );
                assert_eq!(script.unexpected_calls, 0);
                assert!(
                    script.release_statuses.is_empty(),
                    "result was not released"
                );
                assert_eq!(
                    script.calls.iter().filter(|call| **call == XL_FREE).count(),
                    expected.len(),
                    "each callback result must be released exactly once"
                );
            });
        }

        pub(super) fn assert_registration_arguments(&self, expected: &[RegisterArgument]) {
            SCRIPT.with_borrow(|script| {
                assert_eq!(script.registration_arguments, expected);
                assert_eq!(script.registration_arguments_at_release, expected);
            });
        }

        pub(super) fn overwrite_registration_array(&self) {
            SCRIPT.with_borrow_mut(|script| script.overwrite_registration_array = true);
        }

        pub(super) fn on_register(&self, hook: impl FnOnce(&[*mut XLOPER12]) + 'static) {
            SCRIPT.with_borrow_mut(|script| script.registration_hook = Some(Box::new(hook)));
        }

        pub(crate) fn on_call(&self, function: i32, hook: impl FnOnce() + 'static) {
            SCRIPT.with_borrow_mut(|script| {
                assert!(script.call_hook.is_none());
                script.call_hook = Some((function, Box::new(hook)));
            });
        }

        pub(crate) fn on_arguments(
            &self,
            function: i32,
            hook: impl FnOnce(&[*mut XLOPER12]) + 'static,
        ) {
            SCRIPT.with_borrow_mut(|script| {
                script.argument_hooks.push_back((function, Box::new(hook)));
            });
        }
    }

    impl Drop for CallbackScript {
        fn drop(&mut self) {
            crate::test_callback::install();
            crate::module_runtime::reset_callbacks_for_test();
            SCRIPT.with_borrow_mut(|script| *script = ScriptState::default());
        }
    }

    unsafe extern "system" fn callback(
        function: i32,
        argument_count: i32,
        arguments: *mut *mut XLOPER12,
        result: *mut XLOPER12,
    ) -> i32 {
        let argument_hook = SCRIPT.with_borrow_mut(|script| {
            if script
                .argument_hooks
                .front()
                .is_some_and(|(target, _)| *target == function)
            {
                script.argument_hooks.pop_front().map(|(_, hook)| hook)
            } else {
                None
            }
        });
        if let Some(hook) = argument_hook {
            // SAFETY: the invoking host operation keeps this many operand
            // pointers and their payloads live throughout the callback.
            let arguments =
                unsafe { std::slice::from_raw_parts(arguments, argument_count as usize) };
            hook(arguments);
        }
        let hook = SCRIPT.with_borrow_mut(|script| {
            if script
                .call_hook
                .as_ref()
                .is_some_and(|(target, _)| *target == function)
            {
                script.call_hook.take().map(|(_, hook)| hook)
            } else {
                None
            }
        });
        // Release the script borrow before simulating host reentry.
        if let Some(hook) = hook {
            hook();
        }
        if function == XLF_REGISTER {
            let hook = SCRIPT.with_borrow_mut(|script| script.registration_hook.take());
            if let Some(hook) = hook {
                // SAFETY: operands remain live throughout this callback,
                // including while a nested registration allocates and releases.
                let arguments =
                    unsafe { std::slice::from_raw_parts(arguments, argument_count as usize) };
                hook(arguments);
            }
        }
        SCRIPT.with_borrow_mut(|script| {
            script.calls.push(function);
            if function == XL_FREE {
                if !script.registration_pointers.is_empty() {
                    // SAFETY: register retains its temporary operand owners
                    // through release of the callback result.
                    script.registration_arguments_at_release =
                        unsafe { read_registration_arguments(&script.registration_pointers) };
                    script.registration_pointers.clear();
                }
                return match script.release_statuses.pop() {
                    Some(status) => status,
                    None => {
                        script.unexpected_calls += 1;
                        XLRET_FAILED
                    }
                };
            }
            let Some(reply) = script.replies.pop_front() else {
                script.unexpected_calls += 1;
                return XLRET_FAILED;
            };
            script.release_statuses.push(reply.release_status);
            if reply.function != function || result.is_null() {
                script.unexpected_calls += 1;
                return XLRET_FAILED;
            }
            if function == XLF_REGISTER {
                // SAFETY: Excel's callback contract supplies this many live
                // operand pointers for the invocation. Copy their contents
                // before registration drops the temporary UTF-16 owners.
                let operands =
                    unsafe { std::slice::from_raw_parts(arguments, argument_count as usize) };
                // SAFETY: the callback retains each operand through XL_FREE.
                script.registration_arguments = unsafe { read_registration_arguments(operands) };
                script.registration_pointers = operands.to_vec();
                if script.overwrite_registration_array {
                    // SAFETY: the callback owns mutable pointer-array storage;
                    // clearing it does not mutate or invalidate the operands.
                    unsafe { std::slice::from_raw_parts_mut(arguments, argument_count as usize) }
                        .fill(std::ptr::null_mut());
                }
            }
            // SAFETY: the callback supplies writable result storage for this
            // invocation. The test retains any payload referenced in a reply.
            unsafe { *result = reply.value };
            reply.status
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ExcelApiFailure;

    #[test]
    fn registration_name_probe_preserves_excel_namespace_and_utf16_spelling() {
        use super::test_support::{
            CallbackScript, RegisterArgument, Reply, read_registration_arguments,
        };

        const NAME: &str = "計算.Échéance";
        let script =
            CallbackScript::install([Reply::success(XLF_EVALUATE, XLOPER12::error(XLERR_NAME))]);
        script.on_arguments(XLF_EVALUATE, |arguments| {
            // SAFETY: the probe retains the counted name through the callback.
            let values = unsafe { read_registration_arguments(arguments) };
            assert_eq!(values, [RegisterArgument::Text(NAME.to_owned())]);
        });
        let callbacks = HostCallbackSession::new();
        let host = RegistrationHost::new(&callbacks);
        assert!(!host.is_name_bound(NAME).unwrap());
        script.assert_calls(&[XLF_EVALUATE]);
    }

    #[test]
    fn registration_arguments_keep_utf16_owners_live_when_pointer_storage_spills() {
        use super::test_support::{CallbackScript, RegisterArgument, Reply};
        use crate::registration::{ArgumentAbi, ArgumentDescriptor, RegistrationDescriptor};

        const ARGUMENTS: [ArgumentDescriptor; 6] = [
            ArgumentDescriptor {
                name: "a",
                description: "日本語",
            },
            ArgumentDescriptor {
                name: "b",
                description: "This long description spills the temporary UTF-16 inline buffer while remaining valid registration metadata.",
            },
            ArgumentDescriptor {
                name: "c",
                description: "third",
            },
            ArgumentDescriptor {
                name: "d",
                description: "fourth",
            },
            ArgumentDescriptor {
                name: "e",
                description: "fifth",
            },
            ArgumentDescriptor {
                name: "f",
                description: "sixth",
            },
        ];
        const ABIS: [ArgumentAbi; 6] = [ArgumentAbi::Value12; 6];
        let module = "C:\\test\\addin.xll";
        let module_units = module.encode_utf16().collect::<Vec<_>>();
        let prepared_module = CountedModuleName::from_units(&module_units).unwrap();
        let mut cached_text = RegistrationScratch::default();

        // Ten fixed operands plus n help strings and their blank trailer
        // exercise empty, inline-full, and spilled help and pointer storage,
        // then shrink the active help list and reuse the spilled frame.
        for count in [0, 1, 3, 4, 5, 6, 1, 0, 6] {
            let description = if count % 2 == 0 {
                "機能の説明"
            } else {
                "別の説明💡"
            };
            let descriptor = RegistrationDescriptor {
                export_name: "xll_test",
                excel_name: "TEST.REGISTER",
                signature: super::super::RegistrationSignature {
                    return_abi: crate::registration::ReturnAbi::Value12,
                    execution: xlfn_common::ExecutionKind::MainThread,
                    arguments: &ABIS[..count],
                    volatile: false,
                },
                category: "Test",
                description,
                help_topic: "",
                visibility: FunctionVisibility::Public,
                arguments: &ARGUMENTS[..count],
            };
            let prepared = super::super::preflight_registration(&[descriptor]).unwrap();
            let script =
                CallbackScript::install([Reply::success(XLF_REGISTER, XLOPER12::number(42.0))]);
            script.overwrite_registration_array();
            let expected_module_pointer = prepared_module.as_counted().as_ptr().addr();
            script.on_arguments(XLF_REGISTER, move |arguments| {
                // SAFETY: the registration call retains its immutable module
                // storage throughout the host callback.
                let oper = unsafe { &*arguments[0] };
                assert_eq!(oper.xltype, xlfn_sys::XLTYPE_STR);
                let module_pointer = {
                    // SAFETY: The preceding tag check proved XLTYPE_STR, so
                    // this reads the active string union member.
                    unsafe { oper.value.string }
                };
                assert_eq!(module_pointer.addr(), expected_module_pointer);
            });
            let callbacks = HostCallbackSession::new();
            let host = RegistrationHost::new(&callbacks);
            let outcome =
                host.register(&prepared_module, &prepared.as_slice()[0], &mut cached_text);
            assert!(
                matches!(outcome, RegistrationMutation::Applied { value, cleanup: Ok(()) } if value.id == 42.0)
            );

            let text = |value: &str| RegisterArgument::Text(value.to_owned());
            let names = ARGUMENTS[..count]
                .iter()
                .map(|argument| argument.name)
                .collect::<Vec<_>>()
                .join(",");
            let mut expected = vec![
                text(module),
                text("xll_test"),
                text(&"Q".repeat(count + 1)),
                text("TEST.REGISTER"),
                text(&names),
                RegisterArgument::Number(1.0),
                text("Test"),
                text(""),
                text(""),
                text(description),
            ];
            if count > 0 {
                expected.extend(
                    ARGUMENTS[..count]
                        .iter()
                        .map(|argument| text(argument.description)),
                );
                expected.push(text(""));
            }
            script.assert_registration_arguments(&expected);
            script.assert_calls(&[XLF_REGISTER]);
        }
    }

    #[test]
    fn temporary_strings_are_counted_utf16() {
        let mut text = temporary_string("価格").unwrap();
        let pointer = text.pointer();
        // SAFETY: the pointer is non-null and valid for the live temporary.
        let oper = unsafe { &*pointer.as_ptr() };
        // SAFETY: the active string member points at the temporary storage.
        let units = unsafe { oper.value.string };
        // SAFETY: the counted allocation contains the prefix and two units.
        let units = unsafe { std::slice::from_raw_parts(units, 3) };
        assert_eq!(units, &[2, 0x4fa1, 0x683c]);
    }

    #[test]
    fn registration_spills_survive_nested_registration_and_result_release() {
        use super::test_support::{CallbackScript, Reply, read_registration_arguments};
        use crate::registration::{
            ArgumentAbi, ArgumentDescriptor, RegistrationDescriptor, RegistrationSignature,
        };
        use std::cell::Cell;
        use std::rc::Rc;

        let descriptor = RegistrationDescriptor {
            export_name: "nested_registration",
            excel_name: "TEST.NESTED.REGISTRATION",
            signature: RegistrationSignature {
                return_abi: crate::registration::ReturnAbi::Value12,
                execution: xlfn_common::ExecutionKind::MainThread,
                arguments: &[ArgumentAbi::Value12; 6],
                volatile: false,
            },
            category: "Test",
            description: "A long help description spills UTF-16 storage and must remain readable after a nested registration has released its independent storage.",
            help_topic: "https://example.invalid/documentation/registration/lifetime/nested/very-long-help-topic",
            visibility: FunctionVisibility::Public,
            arguments: &[ArgumentDescriptor {
                name: "a",
                description: "This nested argument description spills its own retained text buffer for the many-argument frame.",
            }; 6],
        };
        let prepared = super::super::preflight_registration(&[descriptor]).unwrap();
        let inner = prepared.clone();
        let outer_module = "C:\\outer\\".repeat(500).encode_utf16().collect::<Vec<_>>();
        let inner_module = "C:\\inner\\".repeat(500).encode_utf16().collect::<Vec<_>>();
        let outer_module = CountedModuleName::from_units(&outer_module).unwrap();
        let inner_module = CountedModuleName::from_units(&inner_module).unwrap();
        let script = CallbackScript::install([
            Reply::success(XLF_REGISTER, XLOPER12::number(11.0)),
            Reply::success(XLF_REGISTER, XLOPER12::number(42.0)),
        ]);
        let nested_succeeded = Rc::new(Cell::new(false));
        let operands_survived = Rc::new(Cell::new(false));
        let succeeded = Rc::clone(&nested_succeeded);
        let survived = Rc::clone(&operands_survived);
        script.on_register(move |outer_pointers| {
            // SAFETY: the outer callback keeps all operands live during reentry.
            let before = unsafe { read_registration_arguments(outer_pointers) };
            let callbacks = HostCallbackSession::new();
            succeeded.set(matches!(
                RegistrationHost::new(&callbacks).register(&inner_module, &inner.as_slice()[0], &mut RegistrationScratch::default()),
                RegistrationMutation::Applied { value, cleanup: Ok(()) } if value.id == 11.0
            ));
            // SAFETY: nested result release must not reclaim outer operands.
            let after = unsafe { read_registration_arguments(outer_pointers) };
            survived.set(before == after);
        });
        let callbacks = HostCallbackSession::new();
        let outcome = RegistrationHost::new(&callbacks).register(
            &outer_module,
            &prepared.as_slice()[0],
            &mut RegistrationScratch::default(),
        );
        assert!(
            matches!(outcome, RegistrationMutation::Applied { value, cleanup: Ok(()) } if value.id == 42.0)
        );
        assert!(nested_succeeded.get());
        assert!(operands_survived.get());
        script.assert_calls(&[XLF_REGISTER, XLF_REGISTER]);
    }

    #[test]
    fn registration_ids_accept_both_signs_but_reject_zero_and_nonfinite_values() {
        for invalid in [0.0, -0.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(!valid_registration_id(invalid));
        }
        for valid in [1.0, 1.5, -1.0, -1_678_704_637.0] {
            assert!(valid_registration_id(valid));
        }
    }

    #[test]
    fn negative_registration_id_survives_registration_and_recovery() {
        let expected = RegistrationId {
            id: -1_678_704_637.0,
            excel_name: "BENCH.ALLOC.BYTES",
        };
        let mut result = ExcelCallbackValue::from_raw_for_test(XLOPER12::number(expected.id));
        assert_eq!(
            decode_registration_id(&mut result, expected.excel_name).unwrap(),
            expected
        );
        assert_eq!(
            decode_registration_id_result(&mut result, expected.excel_name).unwrap(),
            Some(expected)
        );
    }

    #[test]
    fn error_results_are_not_registration_ids() {
        let mut result = ExcelCallbackValue::from_raw_for_test(XLOPER12::error(XLERR_NAME));
        assert!(decode_registration_id(&mut result, "TEST.MISSING").is_err());
        assert_eq!(
            decode_registration_id_result(&mut result, "TEST.MISSING").unwrap(),
            None
        );
        let mut result = ExcelCallbackValue::from_raw_for_test(XLOPER12::number(f64::NAN));
        assert!(decode_registration_id(&mut result, "TEST.INVALID").is_err());
        assert!(decode_registration_id_result(&mut result, "TEST.INVALID").is_err());
    }

    #[test]
    fn cleanup_boolean_must_confirm_the_host_mutation() {
        let true_result = ExcelCallbackValue::from_raw_for_test(XLOPER12::boolean(true));
        assert!(read_applied_bool(&true_result, ExcelApiFunction::Unregister).is_ok());

        for raw in [
            XLOPER12::boolean(false),
            XLOPER12::error(XLERR_NAME),
            XLOPER12::number(1.0),
        ] {
            let result = ExcelCallbackValue::from_raw_for_test(raw);
            assert!(matches!(
                (read_applied_bool(&result, ExcelApiFunction::Unregister))
                    .as_ref()
                    .map_err(crate::error::construct::view),
                Err(crate::error::construct::ErrorView::ExcelApi {
                    function: ExcelApiFunction::Unregister,
                    failure: ExcelApiFailure::UnexpectedResult,
                    ..
                })
            ));
        }
    }

    #[test]
    fn event_registration_accepts_nonzero_signed_tokens() {
        for value in [1, 2, i32::MAX, -1, -1_657_274_367, i32::MIN] {
            let result = ExcelCallbackValue::from_raw_for_test(XLOPER12::integer(value));
            assert_eq!(decode_event_registration_id(&result).unwrap(), value);
        }
        for raw in [
            XLOPER12::integer(0),
            XLOPER12::boolean(true),
            XLOPER12::error(XLERR_NAME),
            XLOPER12::number(1.0),
            XLOPER12::nil(),
            XLOPER12::missing(),
        ] {
            let result = ExcelCallbackValue::from_raw_for_test(raw);
            assert!(decode_event_registration_id(&result).is_err());
        }
    }

    #[test]
    #[cfg(any(not(target_os = "windows"), feature = "async", feature = "handles"))]
    fn invalid_event_acknowledgements_leave_host_mutations_indeterminate() {
        use super::test_support::{CallbackScript, Reply};
        for result in [
            XLOPER12::integer(0),
            XLOPER12::number(1.0),
            XLOPER12::boolean(true),
            XLOPER12::nil(),
        ] {
            let script = CallbackScript::install([Reply::success(XL_EVENT_REGISTER, result)]);
            let callbacks = HostCallbackSession::new();
            let host = RegistrationHost::new(&callbacks);
            assert!(matches!(
                host.register_event("test_event_handler", xlfn_sys::XLEVENT_CALCULATION_ENDED),
                RegistrationMutation::Indeterminate {
                    status: ExcelCallbackStatus::Success,
                    ..
                }
            ));
            script.assert_calls(&[XL_EVENT_REGISTER]);
        }
    }

    #[test]
    fn successful_decode_failure_is_indeterminate_for_mutations() {
        let invocation = HostInvocation::<RegistrationId>::Decoded {
            decoded: Err(XllError::CLOSING),
            cleanup: Ok(()),
        };
        assert!(matches!(
            mutation_from_invocation(RegistrationInvocation::<Register> {
                evidence: invocation,
                operation: std::marker::PhantomData,
            }),
            RegistrationMutation::Indeterminate {
                status: ExcelCallbackStatus::Success,
                ..
            }
        ));
    }

    #[test]
    fn terminal_invocation_is_indeterminate_even_when_cleanup_succeeds() {
        use super::test_support::{CallbackScript, Reply};
        let script =
            CallbackScript::install([Reply::success(XLF_UNREGISTER, XLOPER12::boolean(true))
                .status(xlfn_sys::XLRET_ABORT)]);
        let callbacks = HostCallbackSession::new();
        let invocation = invoke_operation(ExcelHost::new(&callbacks), Unregister, &[]);
        assert!(matches!(
            mutation_from_invocation(invocation),
            RegistrationMutation::Indeterminate {
                status: ExcelCallbackStatus::Abort,
                ..
            }
        ));
        script.assert_calls(&[XLF_UNREGISTER]);
    }
}
