//! Registration-specific façade over the typed Excel callback protocol.
//!
//! This module owns the Excel registration ABI: argument encoding, callback
//! invocation, result decoding, and the distinction between an applied,
//! rejected, and indeterminate host mutation.  Transaction policy and
//! recovery journals remain in `registrar.rs` and `recovery.rs`.

use crate::callback_value::ExcelCallbackValue;
use crate::error::{ExcelApiFailure, ExcelApiFunction, InputError};
use crate::host_api::{ExcelHost, HostInvocation};
use crate::host_callback::HostCallbackSession;
use crate::return_abi::ExcelCallbackStatus;
use crate::value::input::PreparedArgument;
use crate::value::input::sealed::ExcelParameterSealed;
use crate::value::{CallContext, ExcelParameter, FromExcel, InputMode, XlValueRef, XlValueType};
use crate::value::{ExcelInputCells, PreparedExcelSequence};
use crate::{XllError, XllResult};
use smallvec::SmallVec;
use std::path::PathBuf;
use std::ptr::NonNull;

use super::RegistrationId;
#[cfg(feature = "async")]
use super::ledger::EventRegistration;
use super::preflight::PreparedRegistration;
use super::schema::FunctionVisibility;
use xlfn_sys::{
    XL_EVENT_REGISTER, XL_GET_NAME, XLERR_NAME, XLF_EVALUATE, XLF_REGISTER, XLF_SET_NAME,
    XLF_UNREGISTER, XLOPER12, XLOPER12Value, XLTYPE_STR,
};

#[cfg(feature = "async")]
pub(crate) const CALCULATION_CANCELED_EVENT: i32 = xlfn_sys::XLEVENT_CALCULATION_CANCELED;

#[cfg(feature = "async")]
pub(crate) const CALCULATION_ENDED_EVENT: i32 = xlfn_sys::XLEVENT_CALCULATION_ENDED;

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
    pub(crate) fn register(
        &self,
        module_units: &[u16],
        descriptor: &PreparedRegistration,
    ) -> RegistrationMutation<RegistrationId> {
        let mut module = match TemporaryString::from_units(module_units) {
            Ok(value) => value,
            Err(error) => return RegistrationMutation::Rejected { error },
        };
        let mut procedure = match TemporaryString::new(descriptor.export_name_text.as_str()) {
            Ok(value) => value,
            Err(error) => return RegistrationMutation::Rejected { error },
        };
        let mut type_text = match TemporaryString::new(descriptor.type_text.as_str()) {
            Ok(value) => value,
            Err(error) => return RegistrationMutation::Rejected { error },
        };
        let mut function_text = match TemporaryString::new(descriptor.excel_name_text.as_str()) {
            Ok(value) => value,
            Err(error) => return RegistrationMutation::Rejected { error },
        };
        let mut arguments = match TemporaryString::new(descriptor.argument_names.as_str()) {
            Ok(value) => value,
            Err(error) => return RegistrationMutation::Rejected { error },
        };
        let mut macro_type = XLOPER12::number(macro_type(descriptor.visibility));
        let mut category = match TemporaryString::new(descriptor.category_text.as_str()) {
            Ok(value) => value,
            Err(error) => return RegistrationMutation::Rejected { error },
        };
        let mut shortcut = match TemporaryString::new("") {
            Ok(value) => value,
            Err(error) => return RegistrationMutation::Rejected { error },
        };
        let mut help_topic = match TemporaryString::new(descriptor.help_topic_text.as_str()) {
            Ok(value) => value,
            Err(error) => return RegistrationMutation::Rejected { error },
        };
        let mut function_help = match TemporaryString::new(descriptor.description_text.as_str()) {
            Ok(value) => value,
            Err(error) => return RegistrationMutation::Rejected { error },
        };
        let mut argument_help = match prepared_argument_help_strings(&descriptor.argument_help) {
            Ok(value) => value,
            Err(error) => return RegistrationMutation::Rejected { error },
        };

        let mut pointers = vec![
            module.pointer(),
            procedure.pointer(),
            type_text.pointer(),
            function_text.pointer(),
            arguments.pointer(),
            NonNull::from_mut(&mut macro_type),
            category.pointer(),
            shortcut.pointer(),
            help_topic.pointer(),
            function_help.pointer(),
        ];
        pointers.extend(argument_help.iter_mut().map(TemporaryString::pointer));

        let invocation = self
            .excel
            .invoke_protocol(XLF_REGISTER, &pointers, |result| {
                decode_registration_id(result, descriptor.excel_name)
            });
        mutation_from_invocation(
            invocation,
            ExcelApiFunction::Register,
            DecodeFailureDisposition::Indeterminate,
        )
    }

    #[cfg(feature = "async")]
    pub(crate) fn register_event(
        &self,
        procedure: &'static str,
        event: i32,
    ) -> RegistrationMutation<EventRegistration> {
        let mut procedure_value = match TemporaryString::new(procedure) {
            Ok(value) => value,
            Err(error) => return RegistrationMutation::Rejected { error },
        };
        let mut event_value = XLOPER12::integer(event);
        let arguments = [
            procedure_value.pointer(),
            NonNull::from_mut(&mut event_value),
        ];
        let invocation = self
            .excel
            .invoke_protocol(XL_EVENT_REGISTER, &arguments, |result| {
                let registration_id = decode_event_registration_id(result)?;
                Ok(EventRegistration {
                    procedure,
                    event,
                    registration_id,
                    unregistered: false,
                })
            });
        mutation_from_invocation(
            invocation,
            ExcelApiFunction::EventRegister,
            DecodeFailureDisposition::Indeterminate,
        )
    }

    pub(crate) fn registration_id(
        &self,
        excel_name: &'static str,
    ) -> XllResult<Option<RegistrationId>> {
        let mut name = TemporaryString::new(excel_name)?;
        let arguments = [name.pointer()];
        self.excel.invoke(
            XLF_EVALUATE,
            ExcelApiFunction::Evaluate,
            &arguments,
            |result| decode_registration_id_result(result, excel_name),
        )
    }

    pub(crate) fn is_registered_name(&self, excel_name: &'static str) -> XllResult<bool> {
        let mut name = TemporaryString::new(excel_name)?;
        let arguments = [name.pointer()];
        self.excel.invoke(
            XLF_EVALUATE,
            ExcelApiFunction::Evaluate,
            &arguments,
            |result| match result.value_type()? {
                XlValueType::Error => {
                    let code = error_code(result)?;
                    Ok(code != XLERR_NAME)
                }
                XlValueType::Number => Ok(result
                    .borrow()
                    .and_then(|value| f64::from_excel(value, "is_registered_name"))
                    .is_ok_and(valid_registration_id)),
                _ => Ok(false),
            },
        )
    }

    pub(crate) fn unregister_registration(
        &self,
        registration: RegistrationId,
    ) -> RegistrationMutation<()> {
        let mut id = XLOPER12::number(registration.id);
        let arguments = [NonNull::from_mut(&mut id)];
        let invocation = self
            .excel
            .invoke_protocol(XLF_UNREGISTER, &arguments, |result| {
                read_applied_bool(result, ExcelApiFunction::Unregister)
            });
        mutation_from_invocation(
            invocation,
            ExcelApiFunction::Unregister,
            DecodeFailureDisposition::Rejected,
        )
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
                        error: XllError::MetadataDebtBindingChanged { name: excel_name },
                    }
                }
            }
            Err(error) => RegistrationMutation::Rejected { error },
        }
    }

    fn delete_name(&self, excel_name: &'static str) -> RegistrationMutation<()> {
        let mut name = match TemporaryString::new(excel_name) {
            Ok(value) => value,
            Err(error) => return RegistrationMutation::Rejected { error },
        };
        let arguments = [name.pointer()];
        let invocation = self
            .excel
            .invoke_protocol(XLF_SET_NAME, &arguments, |result| {
                read_applied_bool(result, ExcelApiFunction::SetName)
            });
        mutation_from_invocation(
            invocation,
            ExcelApiFunction::SetName,
            DecodeFailureDisposition::Rejected,
        )
    }

    pub(crate) fn unregister_event(&self, event: i32) -> RegistrationMutation<()> {
        let mut nil_procedure = XLOPER12::nil();
        let mut event_value = XLOPER12::integer(event);
        let arguments = [
            NonNull::from_mut(&mut nil_procedure),
            NonNull::from_mut(&mut event_value),
        ];
        let invocation = self
            .excel
            .invoke_protocol(XL_EVENT_REGISTER, &arguments, |result| {
                validate_event_unregister_result(result)
            });
        mutation_from_invocation(
            invocation,
            ExcelApiFunction::EventRegister,
            DecodeFailureDisposition::Rejected,
        )
    }
}

#[derive(Clone, Copy)]
enum DecodeFailureDisposition {
    Rejected,
    Indeterminate,
}

fn mutation_from_invocation<T>(
    invocation: HostInvocation<T>,
    function: ExcelApiFunction,
    decode_failure: DecodeFailureDisposition,
) -> RegistrationMutation<T> {
    match invocation {
        HostInvocation::Suppressed { status } => RegistrationMutation::Rejected {
            error: XllError::ExcelApi {
                function,
                failure: ExcelApiFailure::Suppressed(status),
            },
        },
        HostInvocation::Completed {
            status,
            decoded,
            cleanup,
        } => {
            if status.is_terminal() {
                return RegistrationMutation::Indeterminate {
                    status,
                    error: cleanup.err().unwrap_or(XllError::ExcelApi {
                        function,
                        failure: ExcelApiFailure::Status(status),
                    }),
                };
            }
            if status != ExcelCallbackStatus::Success {
                return RegistrationMutation::Rejected {
                    error: cleanup.err().unwrap_or(XllError::ExcelApi {
                        function,
                        failure: ExcelApiFailure::Status(status),
                    }),
                };
            }
            match decoded {
                Some(Ok(value)) => RegistrationMutation::Applied { value, cleanup },
                Some(Err(error)) => {
                    let error = if matches!(decode_failure, DecodeFailureDisposition::Indeterminate)
                    {
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
                None => unreachable!("successful host callbacks always run their decoder"),
            }
        }
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
    let raw = result.raw()?;
    // SAFETY: the caller checked that the result has XLTYPE_ERR.
    Ok(unsafe { raw.value.error })
}

fn read_excel_bool(result: &ExcelCallbackValue<'_>, function: ExcelApiFunction) -> XllResult<bool> {
    if result.value_type()? != XlValueType::Boolean {
        return Err(unexpected_result(function));
    }
    let raw = result.raw()?;
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
    let raw = result.raw()?;
    // SAFETY: XLTYPE_INT selects the integer union member.
    let value = unsafe { raw.value.integer };
    // xlEventRegister reports failure as zero. Live Windows Excel can return
    // a nonzero acknowledgement with the high bit set (e.g. 0x9d380001), so
    // interpreting the signed integer as a positive counter rejects success.
    // Preserve the raw value; event removal is keyed by event, not this value.
    if value == 0 {
        return Err(XllError::ExcelApi {
            function: ExcelApiFunction::EventRegister,
            failure: ExcelApiFailure::InvalidRegistrationId(value),
        });
    }
    Ok(value)
}

fn validate_event_unregister_result(result: &ExcelCallbackValue<'_>) -> XllResult<()> {
    let _ = decode_event_registration_id(result)?;
    Ok(())
}

fn unexpected_result(function: ExcelApiFunction) -> XllError {
    XllError::ExcelApi {
        function,
        failure: ExcelApiFailure::UnexpectedResult,
    }
}

pub(crate) fn valid_registration_id(id: f64) -> bool {
    // Excel registration IDs are opaque numeric tokens, not positive counters.
    // Windows Excel can return a negative ID for a successful xlfRegister call.
    id.is_finite() && id != 0.0
}

fn prepared_argument_help_strings(
    arguments: &[super::preflight::PreparedExcelString],
) -> XllResult<Vec<TemporaryString>> {
    arguments
        .iter()
        .map(|argument| TemporaryString::new(argument.as_str()))
        .collect()
}

const fn macro_type(visibility: FunctionVisibility) -> f64 {
    match visibility {
        FunctionVisibility::Public => 1.0,
        FunctionVisibility::Hidden => 0.0,
    }
}

pub(crate) struct TemporaryString {
    storage: SmallVec<[u16; 64]>,
    oper: XLOPER12,
}

impl TemporaryString {
    pub(crate) fn new(text: &str) -> XllResult<Self> {
        let storage =
            crate::utf16::encode_counted(text, "registration", crate::utf16::EXCEL_STRING_LIMIT)?;
        Ok(Self {
            storage,
            oper: XLOPER12::nil(),
        })
    }

    pub(crate) fn from_units(units: &[u16]) -> XllResult<Self> {
        if units.len() > crate::utf16::EXCEL_STRING_LIMIT {
            return Err(XllError::input(
                "registration",
                InputError::TooLarge {
                    limit: crate::utf16::EXCEL_STRING_LIMIT,
                    actual: units.len(),
                },
            ));
        }
        let mut storage = SmallVec::with_capacity(units.len() + 1);
        storage.push(units.len() as u16);
        storage.extend_from_slice(units);
        Ok(Self {
            storage,
            oper: XLOPER12::nil(),
        })
    }

    pub(crate) fn pointer(&mut self) -> NonNull<XLOPER12> {
        debug_assert_eq!(self.storage.len(), self.storage[0] as usize + 1);
        self.oper = XLOPER12 {
            value: XLOPER12Value {
                string: self.storage.as_mut_ptr(),
            },
            xltype: XLTYPE_STR,
        };
        NonNull::from_mut(&mut self.oper)
    }
}

pub(crate) struct ModuleName {
    pub(crate) path: PathBuf,
    pub(crate) units: Vec<u16>,
}

impl<'call, M: InputMode> ExcelParameterSealed<'call, M> for ModuleName {}

impl<'call, M: InputMode> ExcelParameter<'call, M> for ModuleName {
    type Prepared = ();

    type Elements = PreparedExcelSequence<'call, Self, PreparedArgument<Self, Self::Prepared>>;
    fn prepare_elements(
        cells: ExcelInputCells<'call>,
        context: &CallContext<'call>,
        identity: &mut M::Identity,
    ) -> XllResult<Self::Elements> {
        cells.retain(
            |value, argument| {
                <Self as ExcelParameter<'call, M>>::prepare(value, argument, context, identity)
            },
            PreparedArgument::materialize,
        )
    }
    fn materialize_elements(elements: Self::Elements) -> XllResult<Vec<Self>> {
        elements.materialize()
    }

    fn decode(
        value: XlValueRef<'call>,
        argument: &'static str,
        _: &CallContext,
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
                .map_err(|_| XllError::input(argument, InputError::InvalidUtf16))?,
        );
        Ok(Self { path, units })
    }
}

fn decode_module_name<'call>(value: XlValueRef<'call>) -> XllResult<ModuleName> {
    ModuleName::from_value(value, "module")
}

#[cfg(all(
    test,
    any(not(target_os = "windows"), feature = "async", feature = "handles")
))]
pub(super) mod test_support {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use xlfn_sys::{XL_FREE, XLOPER12, XLRET_FAILED, XLRET_SUCCESS};

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
        release_status: Option<i32>,
        unexpected_calls: usize,
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
                assert_eq!(script.unexpected_calls, 0);
                assert!(script.release_status.is_none(), "result was not released");
                assert_eq!(
                    script.calls.iter().filter(|call| **call == XL_FREE).count(),
                    expected.len(),
                    "each callback result must be released exactly once"
                );
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
        _argument_count: i32,
        _arguments: *mut *mut XLOPER12,
        result: *mut XLOPER12,
    ) -> i32 {
        SCRIPT.with_borrow_mut(|script| {
            script.calls.push(function);
            if function == XL_FREE {
                return match script.release_status.take() {
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
            script.release_status = Some(reply.release_status);
            if reply.function != function || result.is_null() {
                script.unexpected_calls += 1;
                return XLRET_FAILED;
            }
            // SAFETY: the callback supplies writable result storage for this
            // invocation. Test replies contain only immediate scalar values.
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
    fn temporary_strings_are_counted_utf16() {
        let mut text = TemporaryString::new("価格").unwrap();
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
                read_applied_bool(&result, ExcelApiFunction::Unregister),
                Err(XllError::ExcelApi {
                    function: ExcelApiFunction::Unregister,
                    failure: ExcelApiFailure::UnexpectedResult,
                })
            ));
        }
    }

    #[test]
    fn event_registration_and_unregister_accept_nonzero_integer_acknowledgements() {
        for value in [1, 2, -1, -1_657_274_367, i32::MIN, i32::MAX] {
            let result = ExcelCallbackValue::from_raw_for_test(XLOPER12::integer(value));
            assert_eq!(decode_event_registration_id(&result).unwrap(), value);
            assert!(validate_event_unregister_result(&result).is_ok());
        }

        for raw in [
            XLOPER12::integer(0),
            XLOPER12::boolean(true),
            XLOPER12::error(XLERR_NAME),
            XLOPER12::number(1.0),
        ] {
            let result = ExcelCallbackValue::from_raw_for_test(raw);
            assert!(decode_event_registration_id(&result).is_err());
            assert!(validate_event_unregister_result(&result).is_err());
        }
    }

    #[test]
    fn successful_decode_failure_is_indeterminate_for_mutations() {
        let invocation = HostInvocation::<()>::Completed {
            status: ExcelCallbackStatus::Success,
            decoded: Some(Err(XllError::Closing)),
            cleanup: Ok(()),
        };
        assert!(matches!(
            mutation_from_invocation(
                invocation,
                ExcelApiFunction::Register,
                DecodeFailureDisposition::Indeterminate,
            ),
            RegistrationMutation::Indeterminate {
                status: ExcelCallbackStatus::Success,
                ..
            }
        ));
    }

    #[test]
    fn terminal_invocation_is_indeterminate_even_when_cleanup_succeeds() {
        let invocation = HostInvocation::<()>::Completed {
            status: ExcelCallbackStatus::Abort,
            decoded: None,
            cleanup: Ok(()),
        };
        assert!(matches!(
            mutation_from_invocation(
                invocation,
                ExcelApiFunction::Unregister,
                DecodeFailureDisposition::Rejected,
            ),
            RegistrationMutation::Indeterminate {
                status: ExcelCallbackStatus::Abort,
                ..
            }
        ));
    }
}
