use crate::{
    ExcelCallbackValue, ExcelValueRef, FromExcel, InputError, XllError, XllResult,
    return_value::ExcelCallbackStatus,
};
use std::collections::HashSet;
use std::path::PathBuf;
use std::ptr::NonNull;
use xlfn_sys::{
    XL_EVENT_REGISTER, XLERR_NAME, XLF_EVALUATE, XLF_REGISTER, XLF_SET_NAME, XLF_UNREGISTER,
    XLOPER12, XLOPER12Value, XLRET_SUCCESS, XLTYPE_BOOL, XLTYPE_ERR, XLTYPE_INT, XLTYPE_NUM,
    XLTYPE_STR,
};
#[cfg(any(feature = "async", test))]
use xlfn_sys::{XLEVENT_CALCULATION_CANCELED, XLEVENT_CALCULATION_ENDED};

pub const MAX_EXCEL_FUNCTION_ARGUMENTS: usize = 255;
pub const MAX_REGISTER_ARGUMENT_HELP_ENTRIES: usize = 244;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArgumentDescriptor {
    pub name: &'static str,
    pub description: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArgumentAbi {
    CoercedValue,
    RawReference,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResultAbi {
    Xloper,
    AsyncVoid,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RegistrationFlags {
    pub thread_safe: bool,
    pub macro_sheet: bool,
    pub volatile: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FunctionVisibility {
    #[default]
    Public,
    Hidden,
}

impl FunctionVisibility {
    const fn macro_type(self) -> f64 {
        match self {
            Self::Public => 1.0,
            Self::Hidden => 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RegistrationSignature {
    pub result: ResultAbi,
    pub arguments: &'static [ArgumentAbi],
    pub flags: RegistrationFlags,
}

impl RegistrationSignature {
    pub fn encode(self) -> XllResult<String> {
        if self.flags.thread_safe && self.flags.macro_sheet
            || self.arguments.contains(&ArgumentAbi::RawReference) && !self.flags.macro_sheet
        {
            return Err(XllError::Internal {
                diagnostic_id: 0x5245_4753_4947_4E41,
            });
        }
        let mut text = String::with_capacity(self.arguments.len() + 4);
        text.push(match self.result {
            ResultAbi::Xloper => 'Q',
            ResultAbi::AsyncVoid => '>',
        });
        for argument in self.arguments {
            text.push(match argument {
                ArgumentAbi::CoercedValue => 'Q',
                ArgumentAbi::RawReference => 'U',
            });
        }
        if self.result == ResultAbi::AsyncVoid {
            text.push('X');
        }
        if self.flags.macro_sheet {
            text.push('#');
        }
        if self.flags.thread_safe {
            text.push('$');
        }
        if self.flags.volatile {
            text.push('!');
        }
        Ok(text)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RegistrationDescriptor {
    pub export_name: &'static str,
    pub excel_name: &'static str,
    pub signature: RegistrationSignature,
    pub category: &'static str,
    pub description: &'static str,
    pub help_topic: &'static str,
    pub visibility: FunctionVisibility,
    pub arguments: &'static [ArgumentDescriptor],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RegistrationId {
    pub id: f64,
    pub excel_name: &'static str,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PendingRegistration {
    registration: RegistrationId,
    state: RegistrationCleanupState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RegistrationCleanupState {
    Registered,
    Unregistered,
    NameDeleted,
}

impl From<RegistrationId> for PendingRegistration {
    fn from(registration: RegistrationId) -> Self {
        Self {
            registration,
            state: RegistrationCleanupState::Registered,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum CleanupSeverity {
    BestEffort,
    HostMetadataDebt,
    UnloadUnsafe,
}

impl CleanupSeverity {
    #[must_use]
    pub fn is_unload_unsafe(self) -> bool {
        matches!(self, Self::UnloadUnsafe)
    }

    #[must_use]
    pub fn is_metadata_debt(self) -> bool {
        matches!(self, Self::HostMetadataDebt)
    }
}

impl PendingRegistration {
    #[must_use]
    pub(crate) fn cleanup_severity(&self) -> CleanupSeverity {
        match self.state {
            RegistrationCleanupState::Registered => CleanupSeverity::UnloadUnsafe,
            RegistrationCleanupState::Unregistered => CleanupSeverity::HostMetadataDebt,
            RegistrationCleanupState::NameDeleted => CleanupSeverity::BestEffort,
        }
    }
}

pub(crate) struct UnregisterResult<T> {
    pub(crate) succeeded: Vec<T>,
    pub(crate) failed: Vec<(T, XllError)>,
    pub(crate) metadata_debt: Vec<(T, XllError)>,
}

#[derive(Clone, Debug)]
pub(crate) struct UnknownRegistrationState {
    pub(crate) export_name: &'static str,
    pub(crate) excel_name: &'static str,
    pub(crate) recovery_error: XllError,
}

pub(crate) struct RegistrationTransactionError {
    pub(crate) source: Box<XllError>,
    pub(crate) pending_registrations: Vec<PendingRegistration>,
    pub(crate) pending_events: Vec<EventRegistration>,
    pub(crate) metadata_debt: Vec<PendingRegistration>,
    pub(crate) unknown_registrations: Vec<UnknownRegistrationState>,
    /// When true, the Excel host has entered a terminal state (Abort/Uncalced)
    /// and no further C API calls (including rollback) should be attempted.
    pub(crate) terminal: bool,
}

impl RegistrationTransactionError {
    fn new(source: XllError) -> Self {
        Self {
            source: Box::new(source),
            pending_registrations: Vec::new(),
            pending_events: Vec::new(),
            metadata_debt: Vec::new(),
            unknown_registrations: Vec::new(),
            terminal: false,
        }
    }

    fn terminal(source: XllError) -> Self {
        Self {
            source: Box::new(source),
            pending_registrations: Vec::new(),
            pending_events: Vec::new(),
            metadata_debt: Vec::new(),
            unknown_registrations: Vec::new(),
            terminal: true,
        }
    }
}

impl<T> UnregisterResult<T> {
    fn new(capacity: usize) -> Self {
        Self {
            succeeded: Vec::with_capacity(capacity),
            failed: Vec::new(),
            metadata_debt: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EventRegistration {
    procedure: &'static str,
    event: i32,
    registration_id: i32,
    unregistered: bool,
}

pub(crate) fn validate_descriptors(descriptors: &[RegistrationDescriptor]) -> XllResult<()> {
    let mut exports = HashSet::with_capacity(descriptors.len());
    let mut excel_names = HashSet::with_capacity(descriptors.len());
    for descriptor in descriptors {
        let max_arguments = if descriptor.signature.result == ResultAbi::AsyncVoid {
            MAX_EXCEL_FUNCTION_ARGUMENTS - 1
        } else {
            MAX_EXCEL_FUNCTION_ARGUMENTS
        };
        if descriptor.export_name.is_empty()
            || descriptor.excel_name.is_empty()
            || descriptor.arguments.len() > max_arguments
            || !valid_argument_names(descriptor.arguments)
            || !exports.insert(descriptor.export_name.to_ascii_lowercase())
            || !excel_names.insert(descriptor.excel_name.to_ascii_uppercase())
            || descriptor.signature.arguments.len() != descriptor.arguments.len()
            || descriptor.signature.encode().is_err()
        {
            return Err(XllError::Internal {
                diagnostic_id: 0x5245_4749_5354_5259,
            });
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PreparedRegistration {
    pub descriptor_index: usize,
    pub export_name: &'static str,
    pub excel_name: &'static str,
    pub category: &'static str,
    pub help_topic: &'static str,
    pub description: &'static str,
    pub arguments: &'static [ArgumentDescriptor],
    pub signature: RegistrationSignature,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedRegistrationSet {
    pub(crate) prepared: Vec<PreparedRegistration>,
}

pub(crate) fn preflight_registration(
    descriptors: &[RegistrationDescriptor],
) -> XllResult<PreparedRegistrationSet> {
    validate_descriptors(descriptors)?;

    let mut prepared = Vec::with_capacity(descriptors.len());
    for (index, descriptor) in descriptors.iter().enumerate() {
        validate_excel_string(descriptor.export_name)?;
        validate_excel_string(descriptor.excel_name)?;
        validate_excel_string(descriptor.category)?;
        validate_excel_string(descriptor.help_topic)?;
        validate_excel_string(descriptor.description)?;

        for arg in descriptor.arguments {
            validate_excel_string(arg.name)?;
            validate_excel_string(arg.description)?;
        }

        let argument_names = descriptor
            .arguments
            .iter()
            .map(|argument| argument.name)
            .collect::<Vec<_>>()
            .join(",");
        validate_excel_string(&argument_names)?;

        let encoded_sig = descriptor.signature.encode()?;
        validate_excel_string(&encoded_sig)?;

        prepared.push(PreparedRegistration {
            descriptor_index: index,
            export_name: descriptor.export_name,
            excel_name: descriptor.excel_name,
            category: descriptor.category,
            help_topic: descriptor.help_topic,
            description: descriptor.description,
            arguments: descriptor.arguments,
            signature: descriptor.signature,
        });
    }

    Ok(PreparedRegistrationSet { prepared })
}

fn validate_excel_string(s: &str) -> XllResult<()> {
    let utf16_len = s.encode_utf16().count();
    if utf16_len > crate::utf16::EXCEL_STRING_LIMIT || s.contains('\0') {
        Err(XllError::Internal {
            diagnostic_id: 0x5354_5249_4e47_4c45,
        })
    } else {
        Ok(())
    }
}

fn valid_argument_names(arguments: &[ArgumentDescriptor]) -> bool {
    let mut joined_utf16_len = arguments.len().saturating_sub(1);
    for argument in arguments {
        let name = argument.name;
        let utf16_len = name.encode_utf16().count();
        if name.is_empty()
            || name.contains([',', '\0', '\r', '\n'])
            || utf16_len > crate::utf16::EXCEL_STRING_LIMIT
        {
            return false;
        }
        joined_utf16_len = joined_utf16_len.saturating_add(utf16_len);
    }
    joined_utf16_len <= crate::utf16::EXCEL_STRING_LIMIT
}

pub(crate) struct HostRegistrar {
    module_path: PathBuf,
    module_units: Vec<u16>,
}

struct ModuleName {
    path: PathBuf,
    units: Vec<u16>,
}

impl FromExcel for ModuleName {
    fn from_excel(
        value: ExcelValueRef<'_>,
        argument: &'static str,
        _: &crate::CallContext,
    ) -> XllResult<Self> {
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

impl HostRegistrar {
    pub(crate) fn connect() -> XllResult<Self> {
        // SAFETY: no argument pointers are supplied and Excel owns the result.
        let (status, mut result) = unsafe { ExcelCallbackValue::call(xlfn_sys::XL_GET_NAME, &[]) };
        if status != XLRET_SUCCESS {
            return Err(result.try_release().err().unwrap_or(XllError::ExcelApi {
                function: "xlGetName",
                code: status,
            }));
        }

        // SAFETY: Excel returned a live result XLOPER12 for this stack frame.
        let module_name = ModuleName::from_excel(
            result.borrow()?,
            "module",
            &crate::CallContext::without_runtime(),
        );
        result.try_release()?;
        let module_name = module_name?;
        if !module_name.path.is_absolute() {
            return Err(XllError::input(
                "module",
                InputError::Malformed("xlGetName did not return an absolute module path"),
            ));
        }
        Ok(Self {
            module_path: module_name.path,
            module_units: module_name.units,
        })
    }

    #[must_use]
    pub(crate) fn module_path(&self) -> &PathBuf {
        &self.module_path
    }

    pub(crate) fn register_all(
        &self,
        descriptors: &[RegistrationDescriptor],
    ) -> Result<Vec<RegistrationId>, RegistrationTransactionError> {
        register_all_transaction(
            descriptors,
            |descriptor| self.register_one(descriptor),
            Self::unregister_pending,
        )
    }

    #[cfg(feature = "async")]
    pub(crate) fn register_async_events(
        &self,
    ) -> Result<Vec<EventRegistration>, RegistrationTransactionError> {
        register_async_events_transaction(
            |procedure, event| self.register_event(procedure, event),
            Self::unregister_events_detailed,
        )
    }

    #[cfg(feature = "async")]
    fn register_event(
        &self,
        procedure: &'static str,
        event: i32,
    ) -> Result<EventRegistration, RegistrationTransactionError> {
        let mut procedure_value =
            TemporaryString::new(procedure).map_err(RegistrationTransactionError::new)?;
        let mut event_value = XLOPER12::integer(event);
        let arguments = [procedure_value.pointer(), NonNull::from(&mut event_value)];
        // SAFETY: both arguments are live for the callback.
        let (status, mut result) =
            unsafe { ExcelCallbackValue::call(XL_EVENT_REGISTER, &arguments) };
        if status != XLRET_SUCCESS {
            let callback_status = ExcelCallbackStatus::from_raw(status);
            let source = result.try_release().err().unwrap_or(XllError::ExcelApi {
                function: "xlEventRegister",
                code: status,
            });
            let error = if callback_status.is_terminal() {
                RegistrationTransactionError::terminal(source)
            } else {
                RegistrationTransactionError::new(source)
            };
            return Err(error);
        }
        let result_is_integer = result
            .base_type()
            .map_err(RegistrationTransactionError::new)?
            == XLTYPE_INT;
        let registration_id = if result_is_integer {
            // SAFETY: XLTYPE_INT selects the integer union field.
            unsafe {
                result
                    .raw_pointer()
                    .map_err(RegistrationTransactionError::new)?
                    .as_ref()
                    .value
                    .integer
            }
        } else {
            0
        };
        let registration = EventRegistration {
            procedure,
            event,
            registration_id,
            unregistered: false,
        };
        if let Err(error) = result.try_release() {
            return Err(event_release_failure(
                registration,
                error,
                Self::unregister_events_detailed,
            ));
        }
        if !result_is_integer {
            return Err(event_release_failure(
                registration,
                XllError::ExcelApi {
                    function: "xlEventRegister(result)",
                    code: status,
                },
                Self::unregister_events_detailed,
            ));
        }
        if registration_id <= 0 {
            return Err(event_release_failure(
                registration,
                XllError::ExcelApi {
                    function: "xlEventRegister(result)",
                    code: registration_id,
                },
                Self::unregister_events_detailed,
            ));
        }
        Ok(registration)
    }

    fn register_one(
        &self,
        descriptor: &RegistrationDescriptor,
    ) -> Result<RegistrationId, RegistrationTransactionError> {
        let exists = self
            .is_registered_name(descriptor.excel_name)
            .map_err(RegistrationTransactionError::new)?;
        if exists {
            return Err(RegistrationTransactionError::new(
                XllError::RegistrationConflict {
                    name: descriptor.excel_name,
                },
            ));
        }
        let argument_names = descriptor
            .arguments
            .iter()
            .map(|argument| argument.name)
            .collect::<Vec<_>>()
            .join(",");

        let mut module = TemporaryString::from_units(&self.module_units)
            .map_err(RegistrationTransactionError::new)?;
        let mut procedure = TemporaryString::new(descriptor.export_name)
            .map_err(RegistrationTransactionError::new)?;
        let encoded_type_text = descriptor
            .signature
            .encode()
            .map_err(RegistrationTransactionError::new)?;
        let mut type_text =
            TemporaryString::new(&encoded_type_text).map_err(RegistrationTransactionError::new)?;
        let mut function_text = TemporaryString::new(descriptor.excel_name)
            .map_err(RegistrationTransactionError::new)?;
        let mut arguments =
            TemporaryString::new(&argument_names).map_err(RegistrationTransactionError::new)?;
        let mut macro_type = XLOPER12::number(descriptor.visibility.macro_type());
        let mut category =
            TemporaryString::new(descriptor.category).map_err(RegistrationTransactionError::new)?;
        let mut shortcut = TemporaryString::new("").map_err(RegistrationTransactionError::new)?;
        let mut help_topic = TemporaryString::new(descriptor.help_topic)
            .map_err(RegistrationTransactionError::new)?;
        let mut function_help = TemporaryString::new(descriptor.description)
            .map_err(RegistrationTransactionError::new)?;
        let mut argument_help = argument_help_strings(descriptor.arguments)
            .map_err(RegistrationTransactionError::new)?;

        let mut pointers = vec![
            module.pointer(),
            procedure.pointer(),
            type_text.pointer(),
            function_text.pointer(),
            arguments.pointer(),
            NonNull::from(&mut macro_type),
            category.pointer(),
            shortcut.pointer(),
            help_topic.pointer(),
            function_help.pointer(),
        ];
        pointers.extend(argument_help.iter_mut().map(TemporaryString::pointer));

        // SAFETY: every pointer refers to a live stack value or TemporaryString.
        let (status, mut result) = unsafe { ExcelCallbackValue::call(XLF_REGISTER, &pointers) };
        if status != XLRET_SUCCESS {
            let callback_status = ExcelCallbackStatus::from_raw(status);
            let source = result.try_release().err().unwrap_or(XllError::ExcelApi {
                function: "xlfRegister",
                code: status,
            });
            let error = if callback_status.is_terminal() {
                RegistrationTransactionError::terminal(source)
            } else {
                RegistrationTransactionError::new(source)
            };
            return Err(error);
        }
        if result
            .base_type()
            .map_err(RegistrationTransactionError::new)?
            != XLTYPE_NUM
        {
            let base_type = result
                .base_type()
                .map_err(RegistrationTransactionError::new)?;
            let source = result.try_release().err().unwrap_or(XllError::ExcelApi {
                function: "xlfRegister(result)",
                code: base_type as i32,
            });
            return Err(self.reconcile_malformed_registration_result(descriptor, source));
        }
        let id = match result.borrow().and_then(|value| {
            f64::from_excel(
                value,
                "registration",
                &crate::CallContext::without_runtime(),
            )
        }) {
            Ok(id) => id,
            Err(error) => {
                let source = result.try_release().err().unwrap_or(error);
                return Err(self.reconcile_malformed_registration_result(descriptor, source));
            }
        };
        if !valid_registration_id(id) {
            let source = result.try_release().err().unwrap_or(XllError::ExcelApi {
                function: "xlfRegister(result)",
                code: -1,
            });
            return Err(self.reconcile_malformed_registration_result(descriptor, source));
        }
        let registration = RegistrationId {
            id,
            excel_name: descriptor.excel_name,
        };
        if let Err(error) = result.try_release() {
            return Err(registration_release_failure(
                registration,
                error,
                Self::unregister_pending,
            ));
        }
        Ok(registration)
    }

    fn reconcile_malformed_registration_result(
        &self,
        descriptor: &RegistrationDescriptor,
        source: XllError,
    ) -> RegistrationTransactionError {
        reconcile_malformed_registration_result(
            descriptor,
            source,
            |excel_name| self.recover_registration_id(excel_name),
            Self::unregister_pending,
        )
    }

    fn recover_registration_id(
        &self,
        excel_name: &'static str,
    ) -> XllResult<Option<RegistrationId>> {
        let mut name = TemporaryString::new(excel_name)?;
        let arguments = [name.pointer()];
        // SAFETY: the counted name remains live for this synchronous callback.
        let (status, mut result) = unsafe { ExcelCallbackValue::call(XLF_EVALUATE, &arguments) };
        if status != XLRET_SUCCESS {
            return Err(result.try_release().err().unwrap_or(XllError::ExcelApi {
                function: "xlfEvaluate(registration recovery)",
                code: status,
            }));
        }

        if result.base_type()? == XLTYPE_ERR {
            // SAFETY: XLTYPE_ERR selects the error union member.
            let code = unsafe { result.raw_pointer()?.as_ref().value.error };
            result.try_release()?;
            return if code == XLERR_NAME {
                Ok(None)
            } else {
                Err(XllError::ExcelApi {
                    function: "xlfEvaluate(registration recovery result)",
                    code,
                })
            };
        }

        if result.base_type()? != XLTYPE_NUM {
            let base_type = result.base_type()?;
            result.try_release()?;
            return Err(XllError::ExcelApi {
                function: "xlfEvaluate(registration recovery result)",
                code: base_type as i32,
            });
        }

        let id = result.borrow().and_then(|value| {
            f64::from_excel(
                value,
                "registration recovery",
                &crate::CallContext::without_runtime(),
            )
        });
        result.try_release()?;
        let id = id?;
        if !valid_registration_id(id) {
            return Err(XllError::ExcelApi {
                function: "xlfEvaluate(registration recovery result)",
                code: -1,
            });
        }
        Ok(Some(RegistrationId { id, excel_name }))
    }

    fn is_registered_name(&self, excel_name: &'static str) -> XllResult<bool> {
        let mut name = TemporaryString::new(excel_name)?;
        let arguments = [name.pointer()];
        // SAFETY: the name remains live for this synchronous callback.
        let (status, mut result) = unsafe { ExcelCallbackValue::call(XLF_EVALUATE, &arguments) };
        if status != XLRET_SUCCESS {
            return Err(result.try_release().err().unwrap_or(XllError::ExcelApi {
                function: "xlfEvaluate",
                code: status,
            }));
        }
        let is_conflict = if result.base_type()? == XLTYPE_ERR {
            // SAFETY: XLTYPE_ERR selects the error union member.
            unsafe { result.raw_pointer()?.as_ref().value.error != XLERR_NAME }
        } else if result.base_type()? == XLTYPE_NUM {
            let id = result.borrow().and_then(|value| {
                f64::from_excel(
                    value,
                    "is_registered_name",
                    &crate::CallContext::without_runtime(),
                )
            });
            match id {
                Ok(id) => valid_registration_id(id),
                Err(_) => false,
            }
        } else {
            false
        };
        result.try_release()?;
        Ok(is_conflict)
    }
    pub(crate) fn unregister_pending(
        registrations: &[PendingRegistration],
    ) -> UnregisterResult<PendingRegistration> {
        let mut outcome = UnregisterResult::new(registrations.len());
        let mut terminal = false;
        for registration in registrations.iter().rev() {
            let mut registration = registration.clone();
            if terminal {
                // Terminal status detected during rollback: no further C API
                // calls are safe. Record remaining items as failed debt.
                outcome.failed.push((registration, XllError::Closing));
                continue;
            }
            if registration.state == RegistrationCleanupState::NameDeleted {
                outcome.succeeded.push(registration);
                continue;
            }
            if registration.state == RegistrationCleanupState::Registered {
                let mut id = XLOPER12::number(registration.registration.id);
                let arguments = [NonNull::from(&mut id)];
                // SAFETY: id is live for the callback.
                let (status, mut result) =
                    unsafe { ExcelCallbackValue::call(XLF_UNREGISTER, &arguments) };
                if ExcelCallbackStatus::from_raw(status).is_terminal() {
                    terminal = true;
                    outcome.failed.push((
                        registration,
                        XllError::ExcelApi {
                            function: "xlfUnregister",
                            code: status,
                        },
                    ));
                    continue;
                }
                let unregistered = advance_cleanup_state(
                    &mut registration.state,
                    RegistrationCleanupState::Unregistered,
                    status,
                    &result,
                    "xlfUnregister",
                    "xlfUnregister(result)",
                );
                let release = result.try_release();
                if let Err(error) = unregistered {
                    outcome.failed.push((registration, error));
                    continue;
                }
                if let Err(error) = release {
                    outcome.failed.push((registration, error));
                    continue;
                }
            }

            let mut name = match TemporaryString::new(registration.registration.excel_name) {
                Ok(name) => name,
                Err(error) => {
                    outcome.metadata_debt.push((registration, error));
                    continue;
                }
            };
            let name_arguments = [name.pointer()];
            // SAFETY: name is live for the callback.
            let (status, mut result) =
                unsafe { ExcelCallbackValue::call(XLF_SET_NAME, &name_arguments) };
            if ExcelCallbackStatus::from_raw(status).is_terminal() {
                terminal = true;
                outcome.metadata_debt.push((
                    registration,
                    XllError::ExcelApi {
                        function: "xlfSetName",
                        code: status,
                    },
                ));
                continue;
            }
            let name_deleted = advance_cleanup_state(
                &mut registration.state,
                RegistrationCleanupState::NameDeleted,
                status,
                &result,
                "xlfSetName",
                "xlfSetName(result)",
            );
            let release = result.try_release();
            if let Err(error) = name_deleted {
                outcome.metadata_debt.push((registration, error));
                continue;
            }
            if let Err(error) = release {
                outcome.metadata_debt.push((registration, error));
            } else {
                outcome.succeeded.push(registration);
            }
        }
        outcome
    }

    pub(crate) fn unregister_events_detailed(
        registrations: &[EventRegistration],
    ) -> UnregisterResult<EventRegistration> {
        unregister_events_with(registrations, |registration| {
            let mut nil_procedure = XLOPER12::nil();
            let mut event_value = XLOPER12::integer(registration.event);
            let arguments = [
                NonNull::from(&mut nil_procedure),
                NonNull::from(&mut event_value),
            ];
            // SAFETY: both arguments are live for the callback.
            let (status, mut result) =
                unsafe { ExcelCallbackValue::call(XL_EVENT_REGISTER, &arguments) };
            let detached = if status == XLRET_SUCCESS {
                validate_event_unregister_result(&result)
            } else {
                Ok(())
            };
            let release = result.try_release();
            (status, detached, release)
        })
    }
}

fn advance_cleanup_state(
    state: &mut RegistrationCleanupState,
    next: RegistrationCleanupState,
    status: i32,
    result: &ExcelCallbackValue,
    callback_function: &'static str,
    result_function: &'static str,
) -> XllResult<()> {
    if status != XLRET_SUCCESS {
        return Err(XllError::ExcelApi {
            function: callback_function,
            code: status,
        });
    }
    if !read_excel_bool(result, result_function)? {
        return Err(XllError::ExcelApi {
            function: result_function,
            code: 0,
        });
    }
    // Persist the side effect before xlFree is attempted. A result-release
    // failure must not cause the host mutation to be repeated on retry.
    *state = next;
    Ok(())
}

fn read_excel_bool(result: &ExcelCallbackValue, function: &'static str) -> XllResult<bool> {
    let raw = result.raw()?;
    match raw.base_type() {
        XLTYPE_BOOL => {
            // SAFETY: XLTYPE_BOOL selects the boolean union member.
            Ok(unsafe { raw.value.boolean } != 0)
        }
        XLTYPE_ERR => {
            // SAFETY: XLTYPE_ERR selects the error union member.
            let code = unsafe { raw.value.error };
            Err(XllError::ExcelApi { function, code })
        }
        base_type => Err(XllError::ExcelApi {
            function,
            code: base_type as i32,
        }),
    }
}

fn valid_registration_id(id: f64) -> bool {
    id.is_finite() && id > 0.0
}

fn argument_help_strings(arguments: &[ArgumentDescriptor]) -> XllResult<Vec<TemporaryString>> {
    let mut help = arguments
        .iter()
        .take(MAX_REGISTER_ARGUMENT_HELP_ENTRIES)
        .map(|argument| TemporaryString::new(argument.description))
        .collect::<XllResult<Vec<_>>>()?;
    if !help.is_empty() {
        help.push(TemporaryString::new("")?);
    }
    Ok(help)
}

fn validate_event_unregister_result(result: &ExcelCallbackValue) -> XllResult<()> {
    let raw = result.raw()?;
    if raw.base_type() != XLTYPE_INT {
        return Err(XllError::ExcelApi {
            function: "xlEventRegister(unregister result)",
            code: raw.base_type() as i32,
        });
    }
    // SAFETY: XLTYPE_INT selects the integer union member.
    let value = unsafe { raw.value.integer };
    if value <= 0 {
        return Err(XllError::ExcelApi {
            function: "xlEventRegister(unregister result)",
            code: value,
        });
    }
    Ok(())
}

fn unregister_events_with(
    registrations: &[EventRegistration],
    mut unregister: impl FnMut(&EventRegistration) -> (i32, XllResult<()>, XllResult<()>),
) -> UnregisterResult<EventRegistration> {
    let mut outcome = UnregisterResult::new(registrations.len());
    let mut terminal = false;
    for registration in registrations.iter().rev() {
        let mut registration = registration.clone();
        if terminal {
            outcome.failed.push((registration, XllError::Closing));
            continue;
        }
        if registration.unregistered {
            outcome.succeeded.push(registration);
            continue;
        }
        let (status, detached, release) = unregister(&registration);
        if ExcelCallbackStatus::from_raw(status).is_terminal() {
            terminal = true;
            outcome.failed.push((
                registration,
                XllError::ExcelApi {
                    function: "xlEventRegister(unregister)",
                    code: status,
                },
            ));
            continue;
        }
        if status != XLRET_SUCCESS {
            outcome.failed.push((
                registration,
                XllError::ExcelApi {
                    function: "xlEventRegister(unregister)",
                    code: status,
                },
            ));
            continue;
        }

        if let Err(error) = detached {
            outcome.failed.push((registration, error));
            continue;
        }

        // The callback side effect is certified even if releasing its result
        // fails. Never execute the unregister side effect again on a retry.
        registration.unregistered = true;
        if let Err(error) = release {
            outcome.failed.push((registration, error));
        } else {
            outcome.succeeded.push(registration);
        }
    }
    outcome
}

fn register_all_transaction(
    descriptors: &[RegistrationDescriptor],
    mut register: impl FnMut(
        &RegistrationDescriptor,
    ) -> Result<RegistrationId, RegistrationTransactionError>,
    mut unregister: impl FnMut(&[PendingRegistration]) -> UnregisterResult<PendingRegistration>,
) -> Result<Vec<RegistrationId>, RegistrationTransactionError> {
    let mut registered = Vec::with_capacity(descriptors.len());
    for descriptor in descriptors {
        match register(descriptor) {
            Ok(id) => registered.push(id),
            Err(mut error) => {
                let pending: Vec<_> = registered
                    .iter()
                    .copied()
                    .map(PendingRegistration::from)
                    .collect();
                if error.terminal {
                    // Terminal status: no further C API calls are safe.
                    // Record all already-registered items as debt.
                    error.pending_registrations.extend(pending);
                } else {
                    let outcome = unregister(&pending);
                    error
                        .pending_registrations
                        .extend(outcome.failed.into_iter().map(|(entry, _)| entry));
                    error
                        .metadata_debt
                        .extend(outcome.metadata_debt.into_iter().map(|(entry, _)| entry));
                }
                return Err(error);
            }
        }
    }
    Ok(registered)
}

fn reconcile_malformed_registration_result(
    descriptor: &RegistrationDescriptor,
    source: XllError,
    recover: impl FnOnce(&'static str) -> XllResult<Option<RegistrationId>>,
    unregister: impl FnOnce(&[PendingRegistration]) -> UnregisterResult<PendingRegistration>,
) -> RegistrationTransactionError {
    match recover(descriptor.excel_name) {
        Ok(Some(registration)) => registration_release_failure(registration, source, unregister),
        Ok(None) => RegistrationTransactionError::new(source),
        Err(recovery_error) => {
            let mut error = RegistrationTransactionError::new(source);
            error.unknown_registrations.push(UnknownRegistrationState {
                export_name: descriptor.export_name,
                excel_name: descriptor.excel_name,
                recovery_error,
            });
            error
        }
    }
}

fn registration_release_failure(
    registration: RegistrationId,
    source: XllError,
    unregister: impl FnOnce(&[PendingRegistration]) -> UnregisterResult<PendingRegistration>,
) -> RegistrationTransactionError {
    let pending = [PendingRegistration::from(registration)];
    let mut error = RegistrationTransactionError::new(source);
    let outcome = unregister(&pending);
    error.pending_registrations = outcome.failed.into_iter().map(|(entry, _)| entry).collect();
    error.metadata_debt = outcome
        .metadata_debt
        .into_iter()
        .map(|(entry, _)| entry)
        .collect();
    error
}

#[cfg(any(feature = "async", test))]
fn event_release_failure(
    registration: EventRegistration,
    source: XllError,
    unregister: impl FnOnce(&[EventRegistration]) -> UnregisterResult<EventRegistration>,
) -> RegistrationTransactionError {
    let mut error = RegistrationTransactionError::new(source);
    error.pending_events = unregister(&[registration])
        .failed
        .into_iter()
        .map(|(entry, _)| entry)
        .collect();
    error
}

#[cfg(any(feature = "async", test))]
fn register_async_events_transaction(
    mut register: impl FnMut(
        &'static str,
        i32,
    ) -> Result<EventRegistration, RegistrationTransactionError>,
    mut unregister: impl FnMut(&[EventRegistration]) -> UnregisterResult<EventRegistration>,
) -> Result<Vec<EventRegistration>, RegistrationTransactionError> {
    let mut registrations = Vec::with_capacity(2);
    registrations.push(register(
        "__xlfn_calculation_canceled",
        XLEVENT_CALCULATION_CANCELED,
    )?);
    match register("__xlfn_calculation_ended", XLEVENT_CALCULATION_ENDED) {
        Ok(registration) => {
            registrations.push(registration);
            Ok(registrations)
        }
        Err(mut error) => {
            if error.terminal {
                // Terminal status: no further C API calls are safe.
                error.pending_events.extend(registrations);
            } else {
                error.pending_events.extend(
                    unregister(&registrations)
                        .failed
                        .into_iter()
                        .map(|(entry, _)| entry),
                );
            }
            Err(error)
        }
    }
}

struct TemporaryString {
    storage: Vec<u16>,
    oper: XLOPER12,
}

impl TemporaryString {
    fn new(text: &str) -> XllResult<Self> {
        let mut storage =
            crate::utf16::encode_counted(text, "registration", crate::utf16::EXCEL_STRING_LIMIT)?;
        let oper = XLOPER12 {
            value: XLOPER12Value {
                string: storage.as_mut_ptr(),
            },
            xltype: XLTYPE_STR,
        };
        Ok(Self { storage, oper })
    }

    fn from_units(units: &[u16]) -> XllResult<Self> {
        if units.len() > 32_767 {
            return Err(XllError::input(
                "registration",
                InputError::TooLarge {
                    limit: 32_767,
                    actual: units.len(),
                },
            ));
        }
        let mut storage = Vec::with_capacity(units.len() + 1);
        storage.push(units.len() as u16);
        storage.extend_from_slice(units);
        let oper = XLOPER12 {
            value: XLOPER12Value {
                string: storage.as_mut_ptr(),
            },
            xltype: XLTYPE_STR,
        };
        Ok(Self { storage, oper })
    }

    fn pointer(&mut self) -> NonNull<XLOPER12> {
        debug_assert_eq!(self.storage.len(), self.storage[0] as usize + 1);
        NonNull::from(&mut self.oper)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temporary_strings_are_counted_utf16() {
        let mut text = TemporaryString::new("価格").unwrap();
        let pointer = text.pointer();
        // SAFETY: pointer and its active string member belong to text.
        let units = unsafe { (*pointer.as_ptr()).value.string };
        // SAFETY: the counted allocation contains the prefix and two units.
        let units = unsafe { std::slice::from_raw_parts(units, 3) };
        assert_eq!(units, &[2, 0x4fa1, 0x683c]);
    }

    #[test]
    fn signatures_encode_canonical_excel_type_text() {
        let signature = |arguments, flags| RegistrationSignature {
            result: ResultAbi::Xloper,
            arguments,
            flags,
        };
        assert_eq!(
            signature(
                &[],
                RegistrationFlags {
                    thread_safe: true,
                    ..Default::default()
                }
            )
            .encode()
            .unwrap(),
            "Q$"
        );
        assert_eq!(
            signature(
                &[ArgumentAbi::CoercedValue],
                RegistrationFlags {
                    thread_safe: true,
                    ..Default::default()
                }
            )
            .encode()
            .unwrap(),
            "QQ$"
        );
        assert_eq!(
            signature(
                &[ArgumentAbi::CoercedValue, ArgumentAbi::CoercedValue],
                RegistrationFlags::default()
            )
            .encode()
            .unwrap(),
            "QQQ"
        );
        assert_eq!(
            signature(
                &[ArgumentAbi::RawReference],
                RegistrationFlags {
                    macro_sheet: true,
                    ..Default::default()
                }
            )
            .encode()
            .unwrap(),
            "QU#"
        );
        assert_eq!(
            signature(
                &[ArgumentAbi::CoercedValue],
                RegistrationFlags {
                    volatile: true,
                    ..Default::default()
                }
            )
            .encode()
            .unwrap(),
            "QQ!"
        );
    }

    #[test]
    fn async_signature_uses_void_hidden_handle_and_thread_safe_flag() {
        let signature = RegistrationSignature {
            result: ResultAbi::AsyncVoid,
            arguments: &[ArgumentAbi::CoercedValue, ArgumentAbi::CoercedValue],
            flags: RegistrationFlags {
                thread_safe: true,
                ..RegistrationFlags::default()
            },
        };
        assert_eq!(signature.encode().unwrap(), ">QQX$");
    }

    #[test]
    fn signatures_reject_illegal_capability_combinations() {
        let raw_without_macro = RegistrationSignature {
            result: ResultAbi::Xloper,
            arguments: &[ArgumentAbi::RawReference],
            flags: RegistrationFlags::default(),
        };
        assert!(raw_without_macro.encode().is_err());
        let macro_thread_safe = RegistrationSignature {
            result: ResultAbi::Xloper,
            arguments: &[],
            flags: RegistrationFlags {
                thread_safe: true,
                macro_sheet: true,
                volatile: false,
            },
        };
        assert!(macro_thread_safe.encode().is_err());
    }

    #[test]
    fn descriptors_reject_duplicate_excel_and_export_names() {
        const ARGUMENTS: &[ArgumentDescriptor] = &[];
        const ABI_ARGUMENTS: &[ArgumentAbi] = &[];
        let descriptor = |export_name, excel_name| RegistrationDescriptor {
            export_name,
            excel_name,
            signature: RegistrationSignature {
                result: ResultAbi::Xloper,
                arguments: ABI_ARGUMENTS,
                flags: RegistrationFlags::default(),
            },
            category: "test",
            description: "test",
            help_topic: "",
            visibility: FunctionVisibility::Public,
            arguments: ARGUMENTS,
        };
        assert!(
            validate_descriptors(&[descriptor("same", "FIRST"), descriptor("SAME", "SECOND"),])
                .is_err()
        );
        assert!(
            validate_descriptors(&[descriptor("first", "SAME"), descriptor("second", "same"),])
                .is_err()
        );
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
    fn descriptor_accepts_excel_argument_limit_and_rejects_one_more() {
        let arguments: &'static [ArgumentDescriptor] = Box::leak(
            vec![
                ArgumentDescriptor {
                    name: "value",
                    description: ""
                };
                MAX_EXCEL_FUNCTION_ARGUMENTS
            ]
            .into_boxed_slice(),
        );
        let abi_arguments: &'static [ArgumentAbi] =
            Box::leak(vec![ArgumentAbi::CoercedValue; arguments.len()].into_boxed_slice());
        let descriptor = RegistrationDescriptor {
            export_name: "limit",
            excel_name: "TEST.LIMIT",
            signature: RegistrationSignature {
                result: ResultAbi::Xloper,
                arguments: abi_arguments,
                flags: RegistrationFlags::default(),
            },
            category: "test",
            description: "test",
            help_topic: "",
            visibility: FunctionVisibility::Hidden,
            arguments,
        };
        assert!(validate_descriptors(&[descriptor]).is_ok());

        let too_many: &'static [ArgumentDescriptor] = Box::leak(
            vec![
                ArgumentDescriptor {
                    name: "value",
                    description: ""
                };
                MAX_EXCEL_FUNCTION_ARGUMENTS + 1
            ]
            .into_boxed_slice(),
        );
        let too_many_abi: &'static [ArgumentAbi] =
            Box::leak(vec![ArgumentAbi::CoercedValue; too_many.len()].into_boxed_slice());
        assert!(
            validate_descriptors(&[RegistrationDescriptor {
                arguments: too_many,
                signature: RegistrationSignature {
                    arguments: too_many_abi,
                    ..descriptor.signature
                },
                ..descriptor
            }])
            .is_err()
        );
        assert_eq!(MAX_REGISTER_ARGUMENT_HELP_ENTRIES, 244);
    }

    #[test]
    fn argument_help_reserves_the_final_callback_slot_for_empty_sentinel() {
        let arguments = vec![
            ArgumentDescriptor {
                name: "value",
                description: "help",
            };
            MAX_EXCEL_FUNCTION_ARGUMENTS
        ];
        let help = argument_help_strings(&arguments).unwrap();
        assert_eq!(help.len(), 245);
        assert_eq!(help.last().unwrap().storage[0], 0);
    }

    #[test]
    fn descriptor_rejects_argument_name_delimiters_and_empty_names() {
        const ABI_ARGUMENTS: &[ArgumentAbi] = &[ArgumentAbi::CoercedValue];
        let descriptor = |name| RegistrationDescriptor {
            export_name: "argument_name",
            excel_name: "TEST.ARGUMENT.NAME",
            signature: RegistrationSignature {
                result: ResultAbi::Xloper,
                arguments: ABI_ARGUMENTS,
                flags: RegistrationFlags::default(),
            },
            category: "test",
            description: "test",
            help_topic: "",
            visibility: FunctionVisibility::Public,
            arguments: Box::leak(
                vec![ArgumentDescriptor {
                    name,
                    description: "",
                }]
                .into_boxed_slice(),
            ),
        };

        for invalid in ["", "start,end", "nul\0name", "line\rbreak", "line\nbreak"] {
            assert!(validate_descriptors(&[descriptor(invalid)]).is_err());
        }
        assert!(validate_descriptors(&[descriptor("valid_name")]).is_ok());
    }

    #[test]
    fn unregister_false_payload_keeps_registration_cleanup_debt() {
        let result = ExcelCallbackValue::from_raw_for_test(XLOPER12::boolean(false));
        let mut state = RegistrationCleanupState::Registered;
        let error = advance_cleanup_state(
            &mut state,
            RegistrationCleanupState::Unregistered,
            XLRET_SUCCESS,
            &result,
            "xlfUnregister",
            "xlfUnregister(result)",
        )
        .unwrap_err();

        assert_eq!(state, RegistrationCleanupState::Registered);
        assert!(matches!(
            error,
            XllError::ExcelApi {
                function: "xlfUnregister(result)",
                code: 0,
            }
        ));
    }

    #[test]
    fn set_name_false_and_error_payloads_keep_name_cleanup_debt() {
        for raw in [XLOPER12::boolean(false), XLOPER12::error(XLERR_NAME)] {
            let result = ExcelCallbackValue::from_raw_for_test(raw);
            let mut state = RegistrationCleanupState::Unregistered;
            assert!(
                advance_cleanup_state(
                    &mut state,
                    RegistrationCleanupState::NameDeleted,
                    XLRET_SUCCESS,
                    &result,
                    "xlfSetName",
                    "xlfSetName(result)",
                )
                .is_err()
            );
            assert_eq!(state, RegistrationCleanupState::Unregistered);
        }
    }

    #[test]
    fn successful_payload_advances_state_before_result_release() {
        let result = ExcelCallbackValue::from_raw_for_test(XLOPER12::boolean(true));
        let mut state = RegistrationCleanupState::Registered;
        advance_cleanup_state(
            &mut state,
            RegistrationCleanupState::Unregistered,
            XLRET_SUCCESS,
            &result,
            "xlfUnregister",
            "xlfUnregister(result)",
        )
        .unwrap();
        assert_eq!(state, RegistrationCleanupState::Unregistered);
    }

    #[test]
    fn second_async_event_failure_rolls_back_the_first_registration() {
        let attempts = std::cell::Cell::new(0);
        let rolled_back = std::cell::RefCell::new(Vec::new());
        let result = register_async_events_transaction(
            |procedure, event| {
                let attempt = attempts.get() + 1;
                attempts.set(attempt);
                if attempt == 2 {
                    Err(RegistrationTransactionError::new(XllError::ExcelApi {
                        function: "injected",
                        code: 32,
                    }))
                } else {
                    Ok(EventRegistration {
                        procedure,
                        event,
                        registration_id: attempt,
                        unregistered: false,
                    })
                }
            },
            |registrations| {
                rolled_back.borrow_mut().extend_from_slice(registrations);
                let mut outcome = UnregisterResult::new(registrations.len());
                outcome.succeeded.extend_from_slice(registrations);
                outcome
            },
        );
        assert!(result.is_err());
        assert_eq!(
            rolled_back.borrow().as_slice(),
            &[EventRegistration {
                procedure: "__xlfn_calculation_canceled",
                event: XLEVENT_CALCULATION_CANCELED,
                registration_id: 1,
                unregistered: false,
            }]
        );
    }

    #[test]
    fn failed_registration_rollback_returns_cleanup_debt() {
        const ARGUMENTS: &[ArgumentDescriptor] = &[];
        const ABI_ARGUMENTS: &[ArgumentAbi] = &[];
        let descriptor = |export_name, excel_name| RegistrationDescriptor {
            export_name,
            excel_name,
            signature: RegistrationSignature {
                result: ResultAbi::Xloper,
                arguments: ABI_ARGUMENTS,
                flags: RegistrationFlags::default(),
            },
            category: "test",
            description: "test",
            help_topic: "",
            visibility: FunctionVisibility::Public,
            arguments: ARGUMENTS,
        };
        let descriptors = [descriptor("first", "FIRST"), descriptor("second", "SECOND")];
        let attempts = std::cell::Cell::new(0);
        let result = register_all_transaction(
            &descriptors,
            |descriptor| {
                let attempt = attempts.get() + 1;
                attempts.set(attempt);
                if attempt == 2 {
                    Err(RegistrationTransactionError::new(XllError::ExcelApi {
                        function: "injected register",
                        code: 32,
                    }))
                } else {
                    Ok(RegistrationId {
                        id: f64::from(attempt),
                        excel_name: descriptor.excel_name,
                    })
                }
            },
            |registrations| {
                let mut outcome = UnregisterResult::new(registrations.len());
                outcome
                    .failed
                    .extend(registrations.iter().cloned().map(|entry| {
                        (
                            entry,
                            XllError::ExcelApi {
                                function: "injected unregister",
                                code: 64,
                            },
                        )
                    }));
                outcome
            },
        )
        .unwrap_err();

        assert_eq!(result.pending_registrations.len(), 1);
        assert_eq!(result.pending_registrations[0].registration.id, 1.0);
    }

    #[test]
    fn failed_registration_rollback_returns_metadata_debt() {
        const ARGUMENTS: &[ArgumentDescriptor] = &[];
        const ABI_ARGUMENTS: &[ArgumentAbi] = &[];
        let descriptor = |export_name, excel_name| RegistrationDescriptor {
            export_name,
            excel_name,
            signature: RegistrationSignature {
                result: ResultAbi::Xloper,
                arguments: ABI_ARGUMENTS,
                flags: RegistrationFlags::default(),
            },
            category: "test",
            description: "test",
            help_topic: "",
            visibility: FunctionVisibility::Public,
            arguments: ARGUMENTS,
        };
        let descriptors = [descriptor("first", "FIRST"), descriptor("second", "SECOND")];
        let attempts = std::cell::Cell::new(0);
        let result = register_all_transaction(
            &descriptors,
            |descriptor| {
                let attempt = attempts.get() + 1;
                attempts.set(attempt);
                if attempt == 2 {
                    Err(RegistrationTransactionError::new(XllError::ExcelApi {
                        function: "injected register",
                        code: 32,
                    }))
                } else {
                    Ok(RegistrationId {
                        id: f64::from(attempt),
                        excel_name: descriptor.excel_name,
                    })
                }
            },
            |registrations| {
                let mut outcome = UnregisterResult::new(registrations.len());
                outcome
                    .metadata_debt
                    .extend(registrations.iter().cloned().map(|entry| {
                        (
                            entry,
                            XllError::ExcelApi {
                                function: "injected set_name",
                                code: 64,
                            },
                        )
                    }));
                outcome
            },
        )
        .unwrap_err();

        assert!(result.pending_registrations.is_empty());
        assert_eq!(result.metadata_debt.len(), 1);
        assert_eq!(result.metadata_debt[0].registration.id, 1.0);
    }

    #[test]
    fn malformed_success_payload_recovers_and_unregisters_the_committed_registration() {
        const ARGUMENTS: &[ArgumentDescriptor] = &[];
        const ABI_ARGUMENTS: &[ArgumentAbi] = &[];
        let descriptor = RegistrationDescriptor {
            export_name: "recovered_export",
            excel_name: "RECOVERED.NAME",
            signature: RegistrationSignature {
                result: ResultAbi::Xloper,
                arguments: ABI_ARGUMENTS,
                flags: RegistrationFlags::default(),
            },
            category: "test",
            description: "test",
            help_topic: "",
            visibility: FunctionVisibility::Public,
            arguments: ARGUMENTS,
        };
        let unregistered = std::cell::Cell::new(None);
        let error = reconcile_malformed_registration_result(
            &descriptor,
            XllError::ExcelApi {
                function: "xlfRegister(result)",
                code: XLTYPE_STR as i32,
            },
            |excel_name| {
                Ok(Some(RegistrationId {
                    id: 42.0,
                    excel_name,
                }))
            },
            |registrations| {
                unregistered.set(Some(registrations[0].registration.id));
                let mut outcome = UnregisterResult::new(registrations.len());
                outcome.succeeded.extend_from_slice(registrations);
                outcome
            },
        );

        assert_eq!(unregistered.get(), Some(42.0));
        assert!(error.pending_registrations.is_empty());
        assert!(error.unknown_registrations.is_empty());
    }

    #[test]
    fn malformed_success_payload_marks_registration_state_unknown_when_recovery_fails() {
        const ARGUMENTS: &[ArgumentDescriptor] = &[];
        const ABI_ARGUMENTS: &[ArgumentAbi] = &[];
        let descriptor = RegistrationDescriptor {
            export_name: "unknown_export",
            excel_name: "UNKNOWN.NAME",
            signature: RegistrationSignature {
                result: ResultAbi::Xloper,
                arguments: ABI_ARGUMENTS,
                flags: RegistrationFlags::default(),
            },
            category: "test",
            description: "test",
            help_topic: "",
            visibility: FunctionVisibility::Public,
            arguments: ARGUMENTS,
        };
        let error = reconcile_malformed_registration_result(
            &descriptor,
            XllError::ExcelApi {
                function: "xlfRegister(result)",
                code: XLTYPE_STR as i32,
            },
            |_| {
                Err(XllError::ExcelApi {
                    function: "xlfEvaluate(registration recovery)",
                    code: 32,
                })
            },
            |_| panic!("an unknown registration must not be treated as recoverable"),
        );

        assert_eq!(error.unknown_registrations.len(), 1);
        assert_eq!(error.unknown_registrations[0].export_name, "unknown_export");
        assert_eq!(error.unknown_registrations[0].excel_name, "UNKNOWN.NAME");
    }

    #[test]
    fn callback_release_failure_returns_cleanup_debt_when_unregister_fails() {
        let registration = RegistrationId {
            id: 7.0,
            excel_name: "RELEASE_FAILURE",
        };
        let result = registration_release_failure(
            registration,
            XllError::Internal { diagnostic_id: 1 },
            |registrations| {
                let mut outcome = UnregisterResult::new(registrations.len());
                outcome.failed.push((
                    registrations[0].clone(),
                    XllError::ExcelApi {
                        function: "injected unregister",
                        code: 64,
                    },
                ));
                outcome
            },
        );

        assert_eq!(result.pending_registrations.len(), 1);
        assert_eq!(result.pending_registrations[0].registration, registration);
    }

    #[test]
    fn failed_async_event_rollback_returns_cleanup_debt() {
        let attempts = std::cell::Cell::new(0);
        let result = register_async_events_transaction(
            |procedure, event| {
                let attempt = attempts.get() + 1;
                attempts.set(attempt);
                if attempt == 2 {
                    Err(RegistrationTransactionError::new(XllError::ExcelApi {
                        function: "injected event register",
                        code: 32,
                    }))
                } else {
                    Ok(EventRegistration {
                        procedure,
                        event,
                        registration_id: attempt,
                        unregistered: false,
                    })
                }
            },
            |registrations| {
                let mut outcome = UnregisterResult::new(registrations.len());
                outcome.failed.push((
                    registrations[0].clone(),
                    XllError::ExcelApi {
                        function: "injected event unregister",
                        code: 64,
                    },
                ));
                outcome
            },
        )
        .unwrap_err();

        assert_eq!(result.pending_events.len(), 1);
        assert_eq!(result.pending_events[0].registration_id, 1);
    }

    #[test]
    fn event_unregister_result_requires_a_positive_integer() {
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
    fn event_unregister_release_failure_does_not_repeat_the_side_effect() {
        let registration = EventRegistration {
            procedure: "event",
            event: XLEVENT_CALCULATION_ENDED,
            registration_id: 1,
            unregistered: false,
        };
        let calls = std::cell::Cell::new(0);
        let first = unregister_events_with(&[registration], |_| {
            calls.set(calls.get() + 1);
            (
                XLRET_SUCCESS,
                Ok(()),
                Err(XllError::ExcelApi {
                    function: "xlFree",
                    code: 32,
                }),
            )
        });
        assert_eq!(first.failed.len(), 1);
        assert!(first.failed[0].0.unregistered);

        let retry = unregister_events_with(
            &[first.failed[0].0.clone()],
            |_| -> (i32, XllResult<()>, XllResult<()>) {
                calls.set(calls.get() + 1);
                (XLRET_SUCCESS, Ok(()), Ok(()))
            },
        );

        assert_eq!(calls.get(), 1);
        assert_eq!(retry.succeeded.len(), 1);
        assert!(retry.succeeded[0].unregistered);
    }

    #[test]
    fn event_unregister_zero_result_does_not_claim_detachment() {
        let registration = EventRegistration {
            procedure: "event",
            event: XLEVENT_CALCULATION_ENDED,
            registration_id: 1,
            unregistered: false,
        };
        let result = unregister_events_with(&[registration], |_| {
            (
                XLRET_SUCCESS,
                Err(XllError::ExcelApi {
                    function: "xlEventRegister(unregister result)",
                    code: 0,
                }),
                Ok(()),
            )
        });

        assert_eq!(result.failed.len(), 1);
        assert!(!result.failed[0].0.unregistered);
    }

    #[test]
    fn event_unregister_malformed_result_does_not_claim_detachment() {
        let registration = EventRegistration {
            procedure: "event",
            event: XLEVENT_CALCULATION_ENDED,
            registration_id: 1,
            unregistered: false,
        };
        let result = unregister_events_with(&[registration], |_| {
            (
                XLRET_SUCCESS,
                Err(XllError::ExcelApi {
                    function: "xlEventRegister(unregister result)",
                    code: xlfn_sys::XLTYPE_BOOL as i32,
                }),
                Ok(()),
            )
        });

        assert_eq!(result.failed.len(), 1);
        assert!(!result.failed[0].0.unregistered);
    }

    #[test]
    fn malformed_event_success_is_rolled_back_and_failed_rollback_becomes_debt() {
        let registration = EventRegistration {
            procedure: "event",
            event: XLEVENT_CALCULATION_ENDED,
            registration_id: 0,
            unregistered: false,
        };
        let rollback_calls = std::cell::Cell::new(0);
        let error = event_release_failure(
            registration.clone(),
            XllError::ExcelApi {
                function: "xlEventRegister(result)",
                code: 0,
            },
            |registrations| {
                rollback_calls.set(rollback_calls.get() + 1);
                let mut outcome = UnregisterResult::new(registrations.len());
                outcome.failed.push((
                    registrations[0].clone(),
                    XllError::ExcelApi {
                        function: "xlEventRegister(unregister)",
                        code: xlfn_sys::XLRET_FAILED,
                    },
                ));
                outcome
            },
        );

        assert_eq!(rollback_calls.get(), 1);
        assert_eq!(error.pending_events, vec![registration]);
    }

    #[test]
    fn cleanup_severity_ordering() {
        assert!(CleanupSeverity::BestEffort < CleanupSeverity::HostMetadataDebt);
        assert!(CleanupSeverity::HostMetadataDebt < CleanupSeverity::UnloadUnsafe);
    }

    #[test]
    fn unregister_result_tracks_metadata_debt_separately_from_failed() {
        let registration = PendingRegistration {
            registration: RegistrationId {
                id: 42.0,
                excel_name: "TEST_FUNC",
            },
            state: RegistrationCleanupState::Unregistered,
        };
        let mut outcome = UnregisterResult::new(1);
        outcome.metadata_debt.push((
            registration.clone(),
            XllError::ExcelApi {
                function: "xlfSetName",
                code: 0,
            },
        ));
        assert!(outcome.failed.is_empty());
        assert_eq!(outcome.metadata_debt.len(), 1);
        assert_eq!(
            outcome.metadata_debt[0].0.state,
            RegistrationCleanupState::Unregistered
        );
    }

    #[test]
    fn pending_registration_cleanup_severity() {
        let mut reg = PendingRegistration::from(RegistrationId {
            id: 1.0,
            excel_name: "FOO",
        });
        assert_eq!(reg.cleanup_severity(), CleanupSeverity::UnloadUnsafe);
        assert!(reg.cleanup_severity().is_unload_unsafe());

        reg.state = RegistrationCleanupState::Unregistered;
        assert_eq!(reg.cleanup_severity(), CleanupSeverity::HostMetadataDebt);
        assert!(reg.cleanup_severity().is_metadata_debt());

        reg.state = RegistrationCleanupState::NameDeleted;
        assert_eq!(reg.cleanup_severity(), CleanupSeverity::BestEffort);
    }
}
