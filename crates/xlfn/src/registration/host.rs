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
use crate::value::input::sealed::ExcelParameterSealed;
use crate::value::{CallContext, ExcelParameter, FromExcel, InputMode, XlValueRef, XlValueType};
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

    pub(crate) fn delete_name(&self, excel_name: &'static str) -> RegistrationMutation<()> {
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

    pub(crate) fn metadata_debt_binding(&self, excel_name: &'static str) -> XllResult<Option<f64>> {
        self.registration_id(excel_name)
            .map(|registration| registration.map(|value| value.id))
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
    if value <= 0 {
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
    id.is_finite() && id > 0.0
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
    fn registration_ids_must_be_positive_and_finite() {
        for invalid in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(!valid_registration_id(invalid));
        }
        assert!(valid_registration_id(1.0));
        assert!(valid_registration_id(1.5));
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
    fn event_unregister_requires_a_positive_integer() {
        let positive = ExcelCallbackValue::from_raw_for_test(XLOPER12::integer(1));
        assert!(validate_event_unregister_result(&positive).is_ok());

        for raw in [
            XLOPER12::integer(0),
            XLOPER12::integer(-1),
            XLOPER12::boolean(true),
            XLOPER12::error(XLERR_NAME),
        ] {
            let result = ExcelCallbackValue::from_raw_for_test(raw);
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
