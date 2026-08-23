use crate::callback_value::ExcelCallbackValue;
use crate::error::{ExcelApiFailure, ExcelApiFunction, InputError};
use crate::value::FromExcel;
use crate::{XllError, XllResult};
use crate::{host_callback::HostCallbackSession, return_value::ExcelCallbackStatus};
use smallvec::SmallVec;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::ptr::NonNull;
use xlfn_sys::{
    XL_EVENT_REGISTER, XLERR_NAME, XLF_EVALUATE, XLF_REGISTER, XLF_SET_NAME, XLF_UNREGISTER,
    XLOPER12, XLOPER12Value, XLTYPE_BOOL, XLTYPE_ERR, XLTYPE_INT, XLTYPE_NUM, XLTYPE_STR,
};
#[cfg(any(feature = "async", test))]
use xlfn_sys::{XLEVENT_CALCULATION_CANCELED, XLEVENT_CALCULATION_ENDED};

pub(crate) mod host;
pub(crate) mod ledger;
pub(crate) mod preflight;
pub(crate) mod schema;

#[cfg(test)]
pub(crate) use ledger::CleanupSeverity;
pub(crate) use ledger::{
    EventRegistration, ExcelNameKey, HostMutationJournal, MetadataDebt, MetadataDebtRetryResult,
    PendingRegistration, RegistrationCertainty, RegistrationCleanupState,
    RegistrationTransactionError, UnknownRegistrationState, UnregisterResult,
};
pub(crate) use preflight::preflight_registration;
#[cfg(test)]
pub(crate) use preflight::validate_descriptors;
pub use schema::{ArgumentAbi, ArgumentDescriptor};
pub(crate) use schema::{
    FunctionVisibility, MAX_EXCEL_FUNCTION_ARGUMENTS, MAX_REGISTER_ARGUMENT_HELP_ENTRIES,
    RegistrationDescriptor, RegistrationFlags, RegistrationId, RegistrationSignature, ResultAbi,
};

pub(crate) struct HostRegistrar {
    module_path: PathBuf,
    module_units: Vec<u16>,
}

impl HostRegistrar {
    pub(crate) fn connect(
        callbacks: &mut HostCallbackSession,
    ) -> Result<Self, RegistrationTransactionError> {
        // SAFETY: no argument pointers are supplied and Excel owns the result.
        let (status, mut result) = unsafe {
            callbacks
                .call(xlfn_sys::XL_GET_NAME, &[])
                .map_err(|suppressed| {
                    RegistrationTransactionError::new(XllError::ExcelApi {
                        function: ExcelApiFunction::GetName,
                        failure: ExcelApiFailure::Suppressed(suppressed.status),
                    })
                })?
        };
        if status != ExcelCallbackStatus::Success {
            return Err(RegistrationTransactionError::new(
                result.try_release().err().unwrap_or(XllError::ExcelApi {
                    function: ExcelApiFunction::GetName,
                    failure: ExcelApiFailure::Status(status),
                }),
            ));
        }

        // SAFETY: Excel returned a live result XLOPER12 for this stack frame.
        let module_name =
            host::decode_module_name(result.borrow().map_err(RegistrationTransactionError::new)?);
        let release = result.try_release();
        let module_name = module_name.map_err(RegistrationTransactionError::new)?;
        release.map_err(RegistrationTransactionError::new)?;
        if !module_name.path.is_absolute() {
            return Err(RegistrationTransactionError::new(XllError::input(
                "module",
                InputError::Malformed("xlGetName did not return an absolute module path"),
            )));
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
        callbacks: &mut HostCallbackSession,
        descriptors: &[RegistrationDescriptor],
    ) -> Result<Vec<RegistrationId>, RegistrationTransactionError> {
        register_all_transaction(
            callbacks,
            descriptors,
            |callbacks, descriptor| self.register_one(callbacks, descriptor),
            Self::unregister_pending,
        )
    }

    #[cfg(feature = "async")]
    pub(crate) fn register_async_events(
        &self,
        callbacks: &mut HostCallbackSession,
    ) -> Result<Vec<EventRegistration>, RegistrationTransactionError> {
        register_async_events_transaction(
            callbacks,
            |callbacks, procedure, event| self.register_event(callbacks, procedure, event),
            Self::unregister_events_detailed,
        )
    }

    #[cfg(feature = "async")]
    fn register_event(
        &self,
        callbacks: &mut HostCallbackSession,
        procedure: &'static str,
        event: i32,
    ) -> Result<EventRegistration, RegistrationTransactionError> {
        let mut procedure_value =
            TemporaryString::new(procedure).map_err(RegistrationTransactionError::new)?;
        let mut event_value = XLOPER12::integer(event);
        let arguments = [
            procedure_value.pointer(),
            NonNull::from_mut(&mut event_value),
        ];
        // SAFETY: both arguments are live for the callback.
        let (status, mut result) = unsafe {
            callbacks
                .call(XL_EVENT_REGISTER, &arguments)
                .map_err(|suppressed| {
                    RegistrationTransactionError::new(XllError::ExcelApi {
                        function: ExcelApiFunction::EventRegister,
                        failure: ExcelApiFailure::Suppressed(suppressed.status),
                    })
                })?
        };
        if status != ExcelCallbackStatus::Success {
            let source = result.try_release().err().unwrap_or(XllError::ExcelApi {
                function: ExcelApiFunction::EventRegister,
                failure: ExcelApiFailure::Status(status),
            });
            let mut error = RegistrationTransactionError::new(source);
            if status.is_terminal() {
                error.journal.pending_events.push(EventRegistration {
                    procedure,
                    event,
                    registration_id: 0,
                    unregistered: false,
                });
            }
            return Err(error);
        }
        let result_is_integer = result
            .base_type()
            .map_err(RegistrationTransactionError::new)?
            == XLTYPE_INT;
        let registration_id = if result_is_integer {
            let pointer = result
                .raw_pointer()
                .map_err(RegistrationTransactionError::new)?;
            // SAFETY: pointer is non-null and points to a live result XLOPER12.
            let reference = unsafe { pointer.as_ref() };
            // SAFETY: XLTYPE_INT selects the integer union field.
            unsafe { reference.value.integer }
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
                callbacks,
                registration,
                error,
                Self::unregister_events_detailed,
            ));
        }
        if !result_is_integer {
            return Err(event_release_failure(
                callbacks,
                registration,
                XllError::ExcelApi {
                    function: ExcelApiFunction::EventRegister,
                    failure: ExcelApiFailure::UnexpectedResult,
                },
                Self::unregister_events_detailed,
            ));
        }
        if registration_id <= 0 {
            return Err(event_release_failure(
                callbacks,
                registration,
                XllError::ExcelApi {
                    function: ExcelApiFunction::EventRegister,
                    failure: ExcelApiFailure::InvalidRegistrationId(registration_id),
                },
                Self::unregister_events_detailed,
            ));
        }
        Ok(registration)
    }

    fn register_one(
        &self,
        callbacks: &mut HostCallbackSession,
        descriptor: &RegistrationDescriptor,
    ) -> Result<RegistrationId, RegistrationTransactionError> {
        let exists = self.is_registered_name(callbacks, descriptor.excel_name)?;
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
            NonNull::from_mut(&mut macro_type),
            category.pointer(),
            shortcut.pointer(),
            help_topic.pointer(),
            function_help.pointer(),
        ];
        pointers.extend(argument_help.iter_mut().map(TemporaryString::pointer));

        // SAFETY: every pointer refers to a live stack value or TemporaryString.
        let (status, mut result) = unsafe {
            callbacks
                .call(XLF_REGISTER, &pointers)
                .map_err(|suppressed| {
                    RegistrationTransactionError::new(XllError::ExcelApi {
                        function: ExcelApiFunction::Register,
                        failure: ExcelApiFailure::Suppressed(suppressed.status),
                    })
                })?
        };
        if status != ExcelCallbackStatus::Success {
            let source = result.try_release().err().unwrap_or(XllError::ExcelApi {
                function: ExcelApiFunction::Register,
                failure: ExcelApiFailure::Status(status),
            });
            let mut error = RegistrationTransactionError::new(source);
            if status.is_terminal() {
                error.journal.mark_unknown(UnknownRegistrationState {
                    export_name: descriptor.export_name,
                    excel_name: descriptor.excel_name,
                    recovery_error: XllError::ExcelApi {
                        function: ExcelApiFunction::Register,
                        failure: ExcelApiFailure::Status(status),
                    },
                });
            }
            return Err(error);
        }
        if result
            .base_type()
            .map_err(RegistrationTransactionError::new)?
            != XLTYPE_NUM
        {
            let source = result.try_release().err().unwrap_or(XllError::ExcelApi {
                function: ExcelApiFunction::Register,
                failure: ExcelApiFailure::UnexpectedResult,
            });
            return Err(self.reconcile_malformed_registration_result(callbacks, descriptor, source));
        }
        let id = match result
            .borrow()
            .and_then(|value| f64::from_excel(value, "registration"))
        {
            Ok(id) => id,
            Err(error) => {
                let source = result.try_release().err().unwrap_or(error);
                return Err(
                    self.reconcile_malformed_registration_result(callbacks, descriptor, source)
                );
            }
        };
        if !valid_registration_id(id) {
            let source = result.try_release().err().unwrap_or(XllError::ExcelApi {
                function: ExcelApiFunction::Register,
                failure: ExcelApiFailure::UnexpectedResult,
            });
            return Err(self.reconcile_malformed_registration_result(callbacks, descriptor, source));
        }
        let registration = RegistrationId {
            id,
            excel_name: descriptor.excel_name,
        };
        if let Err(error) = result.try_release() {
            return Err(registration_release_failure(
                callbacks,
                registration,
                error,
                Self::unregister_pending,
            ));
        }
        Ok(registration)
    }

    fn reconcile_malformed_registration_result(
        &self,
        callbacks: &mut HostCallbackSession,
        descriptor: &RegistrationDescriptor,
        source: XllError,
    ) -> RegistrationTransactionError {
        reconcile_malformed_registration_result(
            callbacks,
            descriptor,
            source,
            |callbacks, excel_name| self.recover_registration_id(callbacks, excel_name),
            Self::unregister_pending,
        )
    }

    fn recover_registration_id(
        &self,
        callbacks: &mut HostCallbackSession,
        excel_name: &'static str,
    ) -> Result<Option<RegistrationId>, RegistrationTransactionError> {
        let mut name =
            TemporaryString::new(excel_name).map_err(RegistrationTransactionError::new)?;
        let arguments = [name.pointer()];
        // SAFETY: the counted name remains live for this synchronous callback.
        let (status, mut result) = unsafe {
            callbacks
                .call(XLF_EVALUATE, &arguments)
                .map_err(|suppressed| {
                    RegistrationTransactionError::new(XllError::ExcelApi {
                        function: ExcelApiFunction::Evaluate,
                        failure: ExcelApiFailure::Suppressed(suppressed.status),
                    })
                })?
        };
        if status != ExcelCallbackStatus::Success {
            return Err(RegistrationTransactionError::new(
                result.try_release().err().unwrap_or(XllError::ExcelApi {
                    function: ExcelApiFunction::Evaluate,
                    failure: ExcelApiFailure::Status(status),
                }),
            ));
        }

        if result
            .base_type()
            .map_err(RegistrationTransactionError::new)?
            == XLTYPE_ERR
        {
            let pointer = result
                .raw_pointer()
                .map_err(RegistrationTransactionError::new)?;
            // SAFETY: pointer is non-null and points to a live result XLOPER12.
            let reference = unsafe { pointer.as_ref() };
            // SAFETY: XLTYPE_ERR selects the error union member.
            let code = unsafe { reference.value.error };
            result
                .try_release()
                .map_err(RegistrationTransactionError::new)?;
            return if code == XLERR_NAME {
                Ok(None)
            } else {
                Err(RegistrationTransactionError::new(XllError::ExcelApi {
                    function: ExcelApiFunction::Evaluate,
                    failure: ExcelApiFailure::UnexpectedResult,
                }))
            };
        }

        if result
            .base_type()
            .map_err(RegistrationTransactionError::new)?
            != XLTYPE_NUM
        {
            result
                .try_release()
                .map_err(RegistrationTransactionError::new)?;
            return Err(RegistrationTransactionError::new(XllError::ExcelApi {
                function: ExcelApiFunction::Evaluate,
                failure: ExcelApiFailure::UnexpectedResult,
            }));
        }

        let id = result
            .borrow()
            .and_then(|value| f64::from_excel(value, "registration recovery"));
        result
            .try_release()
            .map_err(RegistrationTransactionError::new)?;
        let id = id.map_err(RegistrationTransactionError::new)?;
        if !valid_registration_id(id) {
            return Err(RegistrationTransactionError::new(XllError::ExcelApi {
                function: ExcelApiFunction::Evaluate,
                failure: ExcelApiFailure::UnexpectedResult,
            }));
        }
        Ok(Some(RegistrationId { id, excel_name }))
    }

    fn is_registered_name(
        &self,
        callbacks: &mut HostCallbackSession,
        excel_name: &'static str,
    ) -> Result<bool, RegistrationTransactionError> {
        let mut name =
            TemporaryString::new(excel_name).map_err(RegistrationTransactionError::new)?;
        let arguments = [name.pointer()];
        // SAFETY: the name remains live for this synchronous callback.
        let (status, mut result) = unsafe {
            callbacks
                .call(XLF_EVALUATE, &arguments)
                .map_err(|suppressed| {
                    RegistrationTransactionError::new(XllError::ExcelApi {
                        function: ExcelApiFunction::Evaluate,
                        failure: ExcelApiFailure::Suppressed(suppressed.status),
                    })
                })?
        };
        if status != ExcelCallbackStatus::Success {
            return Err(RegistrationTransactionError::new(
                result.try_release().err().unwrap_or(XllError::ExcelApi {
                    function: ExcelApiFunction::Evaluate,
                    failure: ExcelApiFailure::Status(status),
                }),
            ));
        }
        let is_conflict = if result
            .base_type()
            .map_err(RegistrationTransactionError::new)?
            == XLTYPE_ERR
        {
            let pointer = result
                .raw_pointer()
                .map_err(RegistrationTransactionError::new)?;
            // SAFETY: pointer is non-null and points to a live result XLOPER12.
            let reference = unsafe { pointer.as_ref() };
            // SAFETY: XLTYPE_ERR selects the error union member.
            unsafe { reference.value.error != XLERR_NAME }
        } else if result
            .base_type()
            .map_err(RegistrationTransactionError::new)?
            == XLTYPE_NUM
        {
            let id = result
                .borrow()
                .and_then(|value| f64::from_excel(value, "is_registered_name"));
            match id {
                Ok(id) => valid_registration_id(id),
                Err(_) => false,
            }
        } else {
            false
        };
        result
            .try_release()
            .map_err(RegistrationTransactionError::new)?;
        Ok(is_conflict)
    }
    pub(crate) fn unregister_pending(
        callbacks: &mut HostCallbackSession,
        registrations: &[PendingRegistration],
    ) -> UnregisterResult<PendingRegistration> {
        let mut outcome = UnregisterResult::new(registrations.len());
        for registration in registrations.iter().rev() {
            let mut registration = registration.clone();
            if !callbacks.permits_callbacks() {
                outcome.failed.push((registration, XllError::Closing));
                continue;
            }
            if registration.state == RegistrationCleanupState::NameDeleted {
                outcome.succeeded.push(registration);
                continue;
            }
            if registration.state == RegistrationCleanupState::Registered {
                let mut id = XLOPER12::number(registration.registration.id);
                let arguments = [NonNull::from_mut(&mut id)];
                // SAFETY: id is live for the callback.
                let (status, mut result) =
                    match unsafe { callbacks.call(XLF_UNREGISTER, &arguments) } {
                        Ok(call) => call,
                        Err(suppressed) => {
                            outcome.failed.push((
                                registration,
                                XllError::ExcelApi {
                                    function: ExcelApiFunction::Unregister,
                                    failure: ExcelApiFailure::Suppressed(suppressed.status),
                                },
                            ));
                            continue;
                        }
                    };
                if status.is_terminal() {
                    outcome.failed.push((
                        registration,
                        XllError::ExcelApi {
                            function: ExcelApiFunction::Unregister,
                            failure: ExcelApiFailure::Status(status),
                        },
                    ));
                    continue;
                }
                let unregistered = advance_cleanup_state(
                    &mut registration.state,
                    RegistrationCleanupState::Unregistered,
                    status,
                    &result,
                    ExcelApiFunction::Unregister,
                );
                let release = result.try_release();
                if let Err(error) = unregistered {
                    outcome.failed.push((registration, error));
                    continue;
                }
                if let Err(error) = release {
                    outcome.cleanup_issues.push(error);
                }
            }

            if !callbacks.permits_callbacks() {
                outcome.metadata_debt.push(MetadataDebt::new(
                    registration.registration,
                    XllError::Closing,
                ));
                continue;
            }

            let mut name = match TemporaryString::new(registration.registration.excel_name) {
                Ok(name) => name,
                Err(error) => {
                    outcome
                        .metadata_debt
                        .push(MetadataDebt::new(registration.registration, error));
                    continue;
                }
            };
            let name_arguments = [name.pointer()];
            // SAFETY: name is live for the callback.
            let (status, mut result) =
                match unsafe { callbacks.call(XLF_SET_NAME, &name_arguments) } {
                    Ok(call) => call,
                    Err(suppressed) => {
                        outcome.metadata_debt.push(MetadataDebt::new(
                            registration.registration,
                            XllError::ExcelApi {
                                function: ExcelApiFunction::SetName,
                                failure: ExcelApiFailure::Suppressed(suppressed.status),
                            },
                        ));
                        continue;
                    }
                };
            if status.is_terminal() {
                outcome.metadata_debt.push(MetadataDebt::new(
                    registration.registration,
                    XllError::ExcelApi {
                        function: ExcelApiFunction::SetName,
                        failure: ExcelApiFailure::Status(status),
                    },
                ));
                continue;
            }
            let name_deleted = advance_cleanup_state(
                &mut registration.state,
                RegistrationCleanupState::NameDeleted,
                status,
                &result,
                ExcelApiFunction::SetName,
            );
            let release = result.try_release();
            if let Err(error) = name_deleted {
                outcome
                    .metadata_debt
                    .push(MetadataDebt::new(registration.registration, error));
                continue;
            }
            if let Err(error) = release {
                outcome.cleanup_issues.push(error);
            }
            outcome.succeeded.push(registration);
        }
        outcome
    }

    pub(crate) fn retry_metadata_debt(
        callbacks: &mut HostCallbackSession,
        debts: &BTreeMap<ExcelNameKey, Vec<MetadataDebt>>,
    ) -> MetadataDebtRetryResult {
        let mut remaining = BTreeMap::new();
        let mut cleanup_issues = Vec::new();
        let mut terminal = None;

        for (key, debt_bucket) in debts {
            if debt_bucket.is_empty() {
                continue;
            }
            if !callbacks.permits_callbacks() {
                if let Some(status) = callbacks.terminal_status() {
                    terminal = Some(XllError::ExcelApi {
                        function: ExcelApiFunction::Evaluate,
                        failure: ExcelApiFailure::Suppressed(status),
                    });
                }
                remaining.insert(key.clone(), debt_bucket.clone());
                remaining.extend(
                    debts
                        .range((std::ops::Bound::Excluded(key), std::ops::Bound::Unbounded))
                        .map(|(later_key, later_debt)| (later_key.clone(), later_debt.clone())),
                );
                break;
            }

            let probe = &debt_bucket[0];
            let current_registration =
                match metadata_debt_binding(callbacks, probe.registration.excel_name) {
                    Ok(registration) => registration,
                    Err(error) => {
                        remaining.insert(
                            key.clone(),
                            debt_bucket
                                .iter()
                                .map(|debt| debt.retry_failed(error.clone()))
                                .collect(),
                        );
                        if !callbacks.permits_callbacks() {
                            terminal = Some(error);
                            remaining.extend(
                                debts
                                    .range((
                                        std::ops::Bound::Excluded(key),
                                        std::ops::Bound::Unbounded,
                                    ))
                                    .map(|(later_key, later_debt)| {
                                        (later_key.clone(), later_debt.clone())
                                    }),
                            );
                            break;
                        }
                        continue;
                    }
                };

            let Some(current_registration) = current_registration else {
                // The name is already absent. The cleanup obligation is
                // satisfied without issuing a destructive call.
                continue;
            };

            let Some(matched_debt) = debt_bucket
                .iter()
                .find(|debt| debt.registration.id == current_registration)
            else {
                let error = XllError::MetadataDebtBindingChanged {
                    name: probe.registration.excel_name,
                };
                remaining.insert(
                    key.clone(),
                    debt_bucket
                        .iter()
                        .map(|debt| debt.retry_failed(error.clone()))
                        .collect(),
                );
                continue;
            };

            let mut name = match TemporaryString::new(matched_debt.registration.excel_name) {
                Ok(name) => name,
                Err(error) => {
                    remaining.insert(
                        key.clone(),
                        debt_bucket
                            .iter()
                            .map(|debt| debt.retry_failed(error.clone()))
                            .collect(),
                    );
                    continue;
                }
            };
            let arguments = [name.pointer()];
            // SAFETY: the temporary name remains live for the callback.
            let (status, mut result) = match unsafe { callbacks.call(XLF_SET_NAME, &arguments) } {
                Ok(call) => call,
                Err(suppressed) => {
                    remaining.insert(
                        key.clone(),
                        debt_bucket
                            .iter()
                            .map(|debt| {
                                debt.retry_failed(XllError::ExcelApi {
                                    function: ExcelApiFunction::SetName,
                                    failure: ExcelApiFailure::Suppressed(suppressed.status),
                                })
                            })
                            .collect(),
                    );
                    continue;
                }
            };
            if status.is_terminal() {
                let error = XllError::ExcelApi {
                    function: ExcelApiFunction::SetName,
                    failure: ExcelApiFailure::Status(status),
                };
                if let Err(release_error) = result.try_release() {
                    cleanup_issues.push(release_error);
                }
                remaining.insert(
                    key.clone(),
                    debt_bucket
                        .iter()
                        .map(|debt| debt.retry_failed(error.clone()))
                        .collect(),
                );
                remaining.extend(
                    debts
                        .range((std::ops::Bound::Excluded(key), std::ops::Bound::Unbounded))
                        .map(|(later_key, later_debt)| (later_key.clone(), later_debt.clone())),
                );
                terminal = Some(error);
                break;
            }

            let deleted = metadata_debt_name_result(status, &result);
            if let Err(error) = deleted {
                remaining.insert(
                    key.clone(),
                    debt_bucket
                        .iter()
                        .map(|debt| debt.retry_failed(error.clone()))
                        .collect(),
                );
            }
            if let Err(error) = result.try_release() {
                cleanup_issues.push(error);
            }
        }

        MetadataDebtRetryResult {
            remaining,
            cleanup_issues,
            terminal,
        }
    }

    pub(crate) fn unregister_events_detailed(
        callbacks: &mut HostCallbackSession,
        registrations: &[EventRegistration],
    ) -> UnregisterResult<EventRegistration> {
        unregister_events_with(registrations, |registration| {
            let mut nil_procedure = XLOPER12::nil();
            let mut event_value = XLOPER12::integer(registration.event);
            let arguments = [
                NonNull::from_mut(&mut nil_procedure),
                NonNull::from_mut(&mut event_value),
            ];
            // SAFETY: both arguments are live for the callback.
            let (status, mut result) =
                match unsafe { callbacks.call(XL_EVENT_REGISTER, &arguments) } {
                    Ok(call) => call,
                    Err(suppressed) => {
                        return EventUnregisterAttempt {
                            status: suppressed.status,
                            detached: Ok(()),
                            release: None,
                        };
                    }
                };
            if status.is_terminal() {
                return EventUnregisterAttempt {
                    status,
                    detached: Ok(()),
                    release: None,
                };
            }
            let detached = if status == ExcelCallbackStatus::Success {
                validate_event_unregister_result(&result)
            } else {
                Ok(())
            };
            let release = result.try_release();
            EventUnregisterAttempt {
                status,
                detached,
                release: Some(release),
            }
        })
    }
}

fn metadata_debt_binding(
    callbacks: &mut HostCallbackSession,
    excel_name: &'static str,
) -> XllResult<Option<f64>> {
    let mut name = TemporaryString::new(excel_name)?;
    let arguments = [name.pointer()];
    // SAFETY: the temporary name remains live for this synchronous callback.
    let (status, mut result) = unsafe {
        callbacks
            .call(XLF_EVALUATE, &arguments)
            .map_err(|suppressed| XllError::ExcelApi {
                function: ExcelApiFunction::Evaluate,
                failure: ExcelApiFailure::Suppressed(suppressed.status),
            })?
    };
    if status != ExcelCallbackStatus::Success {
        return Err(result.try_release().err().unwrap_or(XllError::ExcelApi {
            function: ExcelApiFunction::Evaluate,
            failure: ExcelApiFailure::Status(status),
        }));
    }

    let base_type = match result.base_type() {
        Ok(base_type) => base_type,
        Err(error) => {
            let _ = result.try_release();
            return Err(error);
        }
    };
    if base_type == XLTYPE_ERR {
        let code = result.raw_pointer().map(|pointer| {
            // SAFETY: pointer is non-null and points to a live result XLOPER12.
            let reference = unsafe { pointer.as_ref() };
            // SAFETY: XLTYPE_ERR selects the error union member.
            unsafe { reference.value.error }
        });
        let release = result.try_release();
        let code = code?;
        release?;
        if code == XLERR_NAME {
            return Ok(None);
        }
        return Err(XllError::ExcelApi {
            function: ExcelApiFunction::Evaluate,
            failure: ExcelApiFailure::UnexpectedResult,
        });
    }
    if base_type != XLTYPE_NUM {
        result.try_release()?;
        return Err(XllError::ExcelApi {
            function: ExcelApiFunction::Evaluate,
            failure: ExcelApiFailure::UnexpectedResult,
        });
    }

    let id = result
        .borrow()
        .and_then(|value| f64::from_excel(value, "metadata debt binding"));
    let release = result.try_release();
    let id = id?;
    release?;
    if !valid_registration_id(id) {
        return Err(XllError::ExcelApi {
            function: ExcelApiFunction::Evaluate,
            failure: ExcelApiFailure::UnexpectedResult,
        });
    }
    Ok(Some(id))
}

fn advance_cleanup_state(
    state: &mut RegistrationCleanupState,
    next: RegistrationCleanupState,
    status: ExcelCallbackStatus,
    result: &ExcelCallbackValue,
    function: ExcelApiFunction,
) -> XllResult<()> {
    if status != ExcelCallbackStatus::Success {
        return Err(XllError::ExcelApi {
            function,
            failure: ExcelApiFailure::Status(status),
        });
    }
    if !read_excel_bool(result, function)? {
        return Err(XllError::ExcelApi {
            function,
            failure: ExcelApiFailure::UnexpectedResult,
        });
    }
    // Persist the side effect before xlFree is attempted. A result-release
    // failure must not cause the host mutation to be repeated on retry.
    *state = next;
    Ok(())
}

fn metadata_debt_name_result(
    status: ExcelCallbackStatus,
    result: &ExcelCallbackValue,
) -> XllResult<()> {
    let mut state = RegistrationCleanupState::Unregistered;
    advance_cleanup_state(
        &mut state,
        RegistrationCleanupState::NameDeleted,
        status,
        result,
        ExcelApiFunction::SetName,
    )
}

fn read_excel_bool(result: &ExcelCallbackValue, function: ExcelApiFunction) -> XllResult<bool> {
    let raw = result.raw()?;
    match raw.base_type() {
        XLTYPE_BOOL => {
            // SAFETY: XLTYPE_BOOL selects the boolean union member.
            Ok(unsafe { raw.value.boolean } != 0)
        }
        XLTYPE_ERR => {
            // SAFETY: XLTYPE_ERR selects the error union member.
            let _code = unsafe { raw.value.error };
            Err(XllError::ExcelApi {
                function,
                failure: ExcelApiFailure::UnexpectedResult,
            })
        }
        _ => Err(XllError::ExcelApi {
            function,
            failure: ExcelApiFailure::UnexpectedResult,
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
            function: ExcelApiFunction::EventRegister,
            failure: ExcelApiFailure::UnexpectedResult,
        });
    }
    // SAFETY: XLTYPE_INT selects the integer union member.
    let value = unsafe { raw.value.integer };
    if value <= 0 {
        return Err(XllError::ExcelApi {
            function: ExcelApiFunction::EventRegister,
            failure: ExcelApiFailure::InvalidRegistrationId(value),
        });
    }
    Ok(())
}

struct EventUnregisterAttempt {
    status: ExcelCallbackStatus,
    detached: XllResult<()>,
    release: Option<XllResult<()>>,
}

fn unregister_events_with(
    registrations: &[EventRegistration],
    mut unregister: impl FnMut(&EventRegistration) -> EventUnregisterAttempt,
) -> UnregisterResult<EventRegistration> {
    let mut outcome = UnregisterResult::new(registrations.len());
    for registration in registrations.iter().rev() {
        let mut registration = registration.clone();
        if registration.unregistered {
            outcome.succeeded.push(registration);
            continue;
        }
        let attempt = unregister(&registration);
        if attempt.status.is_terminal() {
            outcome.failed.push((
                registration,
                XllError::ExcelApi {
                    function: ExcelApiFunction::EventRegister,
                    failure: ExcelApiFailure::Status(attempt.status),
                },
            ));
            continue;
        }
        if attempt.status != ExcelCallbackStatus::Success {
            outcome.failed.push((
                registration,
                XllError::ExcelApi {
                    function: ExcelApiFunction::EventRegister,
                    failure: ExcelApiFailure::Status(attempt.status),
                },
            ));
            continue;
        }

        if let Err(error) = attempt.detached {
            outcome.failed.push((registration, error));
            continue;
        }

        // The callback side effect is certified even if releasing its result
        // fails. Never execute the unregister side effect again on a retry.
        registration.unregistered = true;
        if let Some(Err(error)) = attempt.release {
            outcome.cleanup_issues.push(error);
        }
        outcome.succeeded.push(registration);
    }
    outcome
}

fn register_all_transaction(
    callbacks: &mut HostCallbackSession,
    descriptors: &[RegistrationDescriptor],
    mut register: impl FnMut(
        &mut HostCallbackSession,
        &RegistrationDescriptor,
    ) -> Result<RegistrationId, RegistrationTransactionError>,
    mut unregister: impl FnMut(
        &mut HostCallbackSession,
        &[PendingRegistration],
    ) -> UnregisterResult<PendingRegistration>,
) -> Result<Vec<RegistrationId>, RegistrationTransactionError> {
    let mut registered = Vec::with_capacity(descriptors.len());
    for descriptor in descriptors {
        match register(callbacks, descriptor) {
            Ok(id) => registered.push(id),
            Err(mut error) => {
                let pending: Vec<_> = registered
                    .iter()
                    .copied()
                    .map(PendingRegistration::from)
                    .collect();
                if !callbacks.permits_callbacks() {
                    // A terminal status suppresses rollback as well. Preserve
                    // every already-registered item as host cleanup debt.
                    error.journal.pending_registrations.extend(pending);
                } else {
                    let outcome = unregister(callbacks, &pending);
                    error
                        .journal
                        .pending_registrations
                        .extend(outcome.failed.into_iter().map(|(entry, _)| entry));
                    error.journal.metadata_debt.extend(outcome.metadata_debt);
                }
                return Err(error);
            }
        }
    }
    Ok(registered)
}

fn reconcile_malformed_registration_result(
    callbacks: &mut HostCallbackSession,
    descriptor: &RegistrationDescriptor,
    source: XllError,
    recover: impl FnOnce(
        &mut HostCallbackSession,
        &'static str,
    ) -> Result<Option<RegistrationId>, RegistrationTransactionError>,
    unregister: impl FnOnce(
        &mut HostCallbackSession,
        &[PendingRegistration],
    ) -> UnregisterResult<PendingRegistration>,
) -> RegistrationTransactionError {
    match recover(callbacks, descriptor.excel_name) {
        Ok(Some(registration)) => {
            registration_release_failure(callbacks, registration, source, unregister)
        }
        Ok(None) => RegistrationTransactionError::new(source),
        Err(mut recovery_error) => {
            let recovery_source = *recovery_error.source;
            let mut error = RegistrationTransactionError::new(source);
            error
                .journal
                .pending_registrations
                .append(&mut recovery_error.journal.pending_registrations);
            error
                .journal
                .metadata_debt
                .append(&mut recovery_error.journal.metadata_debt);
            error
                .journal
                .pending_events
                .append(&mut recovery_error.journal.pending_events);
            error.journal.mark_unknown(UnknownRegistrationState {
                export_name: descriptor.export_name,
                excel_name: descriptor.excel_name,
                recovery_error: recovery_source,
            });
            error
        }
    }
}

fn registration_release_failure(
    callbacks: &mut HostCallbackSession,
    registration: RegistrationId,
    source: XllError,
    unregister: impl FnOnce(
        &mut HostCallbackSession,
        &[PendingRegistration],
    ) -> UnregisterResult<PendingRegistration>,
) -> RegistrationTransactionError {
    let pending = [PendingRegistration::from(registration)];
    let mut error = RegistrationTransactionError::new(source);
    if !callbacks.permits_callbacks() {
        error
            .journal
            .pending_registrations
            .extend_from_slice(&pending);
        return error;
    }
    let outcome = unregister(callbacks, &pending);
    error.journal.pending_registrations =
        outcome.failed.into_iter().map(|(entry, _)| entry).collect();
    error.journal.metadata_debt = outcome.metadata_debt;
    error
}

#[cfg(any(feature = "async", test))]
fn event_release_failure(
    callbacks: &mut HostCallbackSession,
    registration: EventRegistration,
    source: XllError,
    unregister: impl FnOnce(
        &mut HostCallbackSession,
        &[EventRegistration],
    ) -> UnregisterResult<EventRegistration>,
) -> RegistrationTransactionError {
    let mut error = RegistrationTransactionError::new(source);
    if !callbacks.permits_callbacks() {
        error.journal.pending_events.push(registration);
        return error;
    }
    error.journal.pending_events = unregister(callbacks, &[registration])
        .failed
        .into_iter()
        .map(|(entry, _)| entry)
        .collect();
    error
}

#[cfg(any(feature = "async", test))]
fn register_async_events_transaction(
    callbacks: &mut HostCallbackSession,
    mut register: impl FnMut(
        &mut HostCallbackSession,
        &'static str,
        i32,
    ) -> Result<EventRegistration, RegistrationTransactionError>,
    mut unregister: impl FnMut(
        &mut HostCallbackSession,
        &[EventRegistration],
    ) -> UnregisterResult<EventRegistration>,
) -> Result<Vec<EventRegistration>, RegistrationTransactionError> {
    let mut registrations = Vec::with_capacity(2);
    registrations.push(register(
        callbacks,
        "__xlfn_calculation_canceled",
        XLEVENT_CALCULATION_CANCELED,
    )?);
    match register(
        callbacks,
        "__xlfn_calculation_ended",
        XLEVENT_CALCULATION_ENDED,
    ) {
        Ok(registration) => {
            registrations.push(registration);
            Ok(registrations)
        }
        Err(mut error) => {
            if !callbacks.permits_callbacks() {
                // Terminal status: no further C API calls are safe.
                error.journal.pending_events.extend(registrations);
            } else {
                error.journal.pending_events.extend(
                    unregister(callbacks, &registrations)
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
    storage: SmallVec<[u16; 64]>,
    oper: XLOPER12,
}

impl TemporaryString {
    fn new(text: &str) -> XllResult<Self> {
        let storage =
            crate::utf16::encode_counted(text, "registration", crate::utf16::EXCEL_STRING_LIMIT)?;
        Ok(Self {
            storage,
            oper: XLOPER12::nil(),
        })
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
        let mut storage = SmallVec::with_capacity(units.len() + 1);
        storage.push(units.len() as u16);
        storage.extend_from_slice(units);
        Ok(Self {
            storage,
            oper: XLOPER12::nil(),
        })
    }

    fn pointer(&mut self) -> NonNull<XLOPER12> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temporary_strings_are_counted_utf16() {
        let mut texts = [TemporaryString::new("価格").unwrap()];
        let pointer = texts[0].pointer();
        // SAFETY: pointer is non-null and valid for live temporary string.
        let oper = unsafe { &*pointer.as_ptr() };
        // SAFETY: pointer and its active string member belong to text.
        let units = unsafe { oper.value.string };
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
    fn descriptor_and_debt_name_keys_do_not_apply_inconsistent_unicode_folding() {
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
            validate_descriptors(&[descriptor("upper", "Ä"), descriptor("lower", "ä")]).is_ok()
        );
        let upper = MetadataDebt::new(
            RegistrationId {
                id: 1.0,
                excel_name: "Ä",
            },
            XllError::Closing,
        );
        let lower = MetadataDebt::new(
            RegistrationId {
                id: 2.0,
                excel_name: "ä",
            },
            XllError::Closing,
        );
        assert_ne!(upper.key(), lower.key());
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
            ExcelCallbackStatus::Success,
            &result,
            ExcelApiFunction::Unregister,
        )
        .unwrap_err();

        assert_eq!(state, RegistrationCleanupState::Registered);
        assert!(matches!(
            error,
            XllError::ExcelApi {
                function: ExcelApiFunction::Unregister,
                failure: ExcelApiFailure::UnexpectedResult,
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
                    ExcelCallbackStatus::Success,
                    &result,
                    ExcelApiFunction::SetName,
                )
                .is_err()
            );
            assert_eq!(state, RegistrationCleanupState::Unregistered);
        }
    }

    #[test]
    fn metadata_debt_retry_only_deletes_the_name_and_clears_after_success() {
        let debt = MetadataDebt::new(
            RegistrationId {
                id: 7.0,
                excel_name: "RETRY.NAME",
            },
            XllError::ExcelApi {
                function: ExcelApiFunction::SetName,
                failure: ExcelApiFailure::Status(ExcelCallbackStatus::Abort),
            },
        );
        let failed = debt.retry_failed(XllError::ExcelApi {
            function: ExcelApiFunction::SetName,
            failure: ExcelApiFailure::Status(ExcelCallbackStatus::Uncalced),
        });
        assert_eq!(failed.excel_name(), "RETRY.NAME");
        assert_eq!(failed.attempts(), 2);
        assert_eq!(failed.expected_registration_id(), 7.0);
        assert!(
            metadata_debt_name_result(
                ExcelCallbackStatus::Success,
                &ExcelCallbackValue::from_raw_for_test(XLOPER12::boolean(true)),
            )
            .is_ok()
        );
        assert!(
            metadata_debt_name_result(
                ExcelCallbackStatus::Success,
                &ExcelCallbackValue::from_raw_for_test(XLOPER12::boolean(false)),
            )
            .is_err()
        );
    }

    #[test]
    fn successful_payload_advances_state_before_result_release() {
        let result = ExcelCallbackValue::from_raw_for_test(XLOPER12::boolean(true));
        let mut state = RegistrationCleanupState::Registered;
        assert!(
            advance_cleanup_state(
                &mut state,
                RegistrationCleanupState::Unregistered,
                ExcelCallbackStatus::Success,
                &result,
                ExcelApiFunction::Unregister,
            )
            .is_ok()
        );
        assert_eq!(state, RegistrationCleanupState::Unregistered);
    }

    #[test]
    fn second_async_event_failure_rolls_back_the_first_registration() {
        let attempts = std::cell::Cell::new(0);
        let rolled_back = std::cell::RefCell::new(Vec::new());
        let mut callbacks = HostCallbackSession::new();
        let result = register_async_events_transaction(
            &mut callbacks,
            |_callbacks, procedure, event| {
                let attempt = attempts.get() + 1;
                attempts.set(attempt);
                if attempt == 2 {
                    Err(RegistrationTransactionError::new(XllError::ExcelApi {
                        function: ExcelApiFunction::EventRegister,
                        failure: ExcelApiFailure::Status(ExcelCallbackStatus::Failed(32)),
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
            |_callbacks, registrations| {
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
    fn terminal_async_event_failure_preserves_current_and_previous_events() {
        let attempts = std::cell::Cell::new(0);
        let unregister_calls = std::cell::Cell::new(0);
        let mut callbacks = HostCallbackSession::new();
        let error = register_async_events_transaction(
            &mut callbacks,
            |callbacks, procedure, event| {
                let attempt = attempts.get() + 1;
                attempts.set(attempt);
                if attempt == 2 {
                    callbacks.suppress_for_test(ExcelCallbackStatus::Abort);
                    let mut error = RegistrationTransactionError::new(XllError::ExcelApi {
                        function: ExcelApiFunction::EventRegister,
                        failure: ExcelApiFailure::Status(ExcelCallbackStatus::Abort),
                    });
                    error.journal.pending_events.push(EventRegistration {
                        procedure,
                        event,
                        registration_id: 0,
                        unregistered: false,
                    });
                    Err(error)
                } else {
                    Ok(EventRegistration {
                        procedure,
                        event,
                        registration_id: attempt,
                        unregistered: false,
                    })
                }
            },
            |_callbacks, registrations| {
                unregister_calls.set(unregister_calls.get() + 1);
                let mut outcome = UnregisterResult::new(registrations.len());
                outcome.succeeded.extend_from_slice(registrations);
                outcome
            },
        )
        .unwrap_err();

        assert_eq!(unregister_calls.get(), 0);
        assert_eq!(error.journal.pending_events.len(), 2);
        assert_eq!(error.journal.pending_events[0].registration_id, 0);
        assert_eq!(error.journal.pending_events[1].registration_id, 1);
        assert!(!callbacks.permits_callbacks());
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
        let mut callbacks = HostCallbackSession::new();
        let result = register_all_transaction(
            &mut callbacks,
            &descriptors,
            |_callbacks, descriptor| {
                let attempt = attempts.get() + 1;
                attempts.set(attempt);
                if attempt == 2 {
                    Err(RegistrationTransactionError::new(XllError::ExcelApi {
                        function: ExcelApiFunction::Register,
                        failure: ExcelApiFailure::Status(ExcelCallbackStatus::Failed(32)),
                    }))
                } else {
                    Ok(RegistrationId {
                        id: f64::from(attempt),
                        excel_name: descriptor.excel_name,
                    })
                }
            },
            |_callbacks, registrations| {
                let mut outcome = UnregisterResult::new(registrations.len());
                outcome
                    .failed
                    .extend(registrations.iter().cloned().map(|entry| {
                        (
                            entry,
                            XllError::ExcelApi {
                                function: ExcelApiFunction::Unregister,
                                failure: ExcelApiFailure::Status(ExcelCallbackStatus::Uncalced),
                            },
                        )
                    }));
                outcome
            },
        )
        .unwrap_err();

        assert_eq!(result.journal.pending_registrations.len(), 1);
        assert_eq!(result.journal.pending_registrations[0].registration.id, 1.0);
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
        let mut callbacks = HostCallbackSession::new();
        let result = register_all_transaction(
            &mut callbacks,
            &descriptors,
            |_callbacks, descriptor| {
                let attempt = attempts.get() + 1;
                attempts.set(attempt);
                if attempt == 2 {
                    Err(RegistrationTransactionError::new(XllError::ExcelApi {
                        function: ExcelApiFunction::Register,
                        failure: ExcelApiFailure::Status(ExcelCallbackStatus::Failed(32)),
                    }))
                } else {
                    Ok(RegistrationId {
                        id: f64::from(attempt),
                        excel_name: descriptor.excel_name,
                    })
                }
            },
            |_callbacks, registrations| {
                let mut outcome = UnregisterResult::new(registrations.len());
                outcome
                    .metadata_debt
                    .extend(registrations.iter().map(|entry| {
                        MetadataDebt::new(
                            entry.registration,
                            XllError::ExcelApi {
                                function: ExcelApiFunction::SetName,
                                failure: ExcelApiFailure::Status(ExcelCallbackStatus::Uncalced),
                            },
                        )
                    }));
                outcome
            },
        )
        .unwrap_err();

        assert!(result.journal.pending_registrations.is_empty());
        assert_eq!(result.journal.metadata_debt.len(), 1);
        assert_eq!(result.journal.metadata_debt[0].excel_name(), "FIRST");
    }

    #[test]
    fn terminal_registration_failure_preserves_debt_without_rollback() {
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
        let unregister_calls = std::cell::Cell::new(0);
        let mut callbacks = HostCallbackSession::new();
        let error = register_all_transaction(
            &mut callbacks,
            &descriptors,
            |callbacks, descriptor| {
                let attempt = attempts.get() + 1;
                attempts.set(attempt);
                if attempt == 2 {
                    callbacks.suppress_for_test(ExcelCallbackStatus::Abort);
                    let mut error = RegistrationTransactionError::new(XllError::ExcelApi {
                        function: ExcelApiFunction::Register,
                        failure: ExcelApiFailure::Status(ExcelCallbackStatus::Abort),
                    });
                    error.journal.mark_unknown(UnknownRegistrationState {
                        export_name: descriptor.export_name,
                        excel_name: descriptor.excel_name,
                        recovery_error: XllError::ExcelApi {
                            function: ExcelApiFunction::Register,
                            failure: ExcelApiFailure::Status(ExcelCallbackStatus::Abort),
                        },
                    });
                    Err(error)
                } else {
                    Ok(RegistrationId {
                        id: f64::from(attempt),
                        excel_name: descriptor.excel_name,
                    })
                }
            },
            |_callbacks, registrations| {
                unregister_calls.set(unregister_calls.get() + 1);
                let mut outcome = UnregisterResult::new(registrations.len());
                outcome.succeeded.extend_from_slice(registrations);
                outcome
            },
        )
        .unwrap_err();

        assert_eq!(unregister_calls.get(), 0);
        assert_eq!(error.journal.pending_registrations.len(), 1);
        assert_eq!(error.journal.pending_registrations[0].registration.id, 1.0);
        assert_eq!(error.journal.unknown_registrations.len(), 1);
        assert!(!callbacks.permits_callbacks());
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
        let mut callbacks = HostCallbackSession::new();
        let error = reconcile_malformed_registration_result(
            &mut callbacks,
            &descriptor,
            XllError::ExcelApi {
                function: ExcelApiFunction::Register,
                failure: ExcelApiFailure::UnexpectedResult,
            },
            |_callbacks, excel_name| {
                Ok(Some(RegistrationId {
                    id: 42.0,
                    excel_name,
                }))
            },
            |_callbacks, registrations| {
                unregistered.set(Some(registrations[0].registration.id));
                let mut outcome = UnregisterResult::new(registrations.len());
                outcome.succeeded.extend_from_slice(registrations);
                outcome
            },
        );

        assert_eq!(unregistered.get(), Some(42.0));
        assert!(error.journal.pending_registrations.is_empty());
        assert!(error.journal.unknown_registrations.is_empty());
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
        let mut callbacks = HostCallbackSession::new();
        let error = reconcile_malformed_registration_result(
            &mut callbacks,
            &descriptor,
            XllError::ExcelApi {
                function: ExcelApiFunction::Register,
                failure: ExcelApiFailure::UnexpectedResult,
            },
            |_callbacks, _| {
                Err(RegistrationTransactionError::new(XllError::ExcelApi {
                    function: ExcelApiFunction::Evaluate,
                    failure: ExcelApiFailure::Status(ExcelCallbackStatus::Failed(32)),
                }))
            },
            |_callbacks, _| panic!("an unknown registration must not be treated as recoverable"),
        );

        assert_eq!(error.journal.unknown_registrations.len(), 1);
        assert_eq!(
            error.journal.unknown_registrations[0].export_name,
            "unknown_export"
        );
        assert_eq!(
            error.journal.unknown_registrations[0].excel_name,
            "UNKNOWN.NAME"
        );
    }

    #[test]
    fn terminal_registration_recovery_preserves_unknown_state() {
        const ARGUMENTS: &[ArgumentDescriptor] = &[];
        const ABI_ARGUMENTS: &[ArgumentAbi] = &[];
        let descriptor = RegistrationDescriptor {
            export_name: "terminal_recovery_export",
            excel_name: "TERMINAL.RECOVERY",
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
        let mut callbacks = HostCallbackSession::new();
        let error = reconcile_malformed_registration_result(
            &mut callbacks,
            &descriptor,
            XllError::ExcelApi {
                function: ExcelApiFunction::Register,
                failure: ExcelApiFailure::UnexpectedResult,
            },
            |callbacks, _| {
                callbacks.suppress_for_test(ExcelCallbackStatus::Uncalced);
                Err(RegistrationTransactionError::new(XllError::ExcelApi {
                    function: ExcelApiFunction::Evaluate,
                    failure: ExcelApiFailure::Status(ExcelCallbackStatus::Uncalced),
                }))
            },
            |_callbacks, _| panic!("terminal recovery must not attempt unregister"),
        );

        assert_eq!(error.journal.unknown_registrations.len(), 1);
        assert_eq!(
            error.journal.unknown_registrations[0].excel_name,
            "TERMINAL.RECOVERY"
        );
        assert!(!callbacks.permits_callbacks());
    }

    #[test]
    fn callback_release_failure_returns_cleanup_debt_when_unregister_fails() {
        let registration = RegistrationId {
            id: 7.0,
            excel_name: "RELEASE_FAILURE",
        };
        let mut callbacks = HostCallbackSession::new();
        let result = registration_release_failure(
            &mut callbacks,
            registration,
            XllError::Internal {
                diagnostic_id: crate::error::DiagnosticId::from_u64(1),
            },
            |_callbacks, registrations| {
                let mut outcome = UnregisterResult::new(registrations.len());
                outcome.failed.push((
                    registrations[0].clone(),
                    XllError::ExcelApi {
                        function: ExcelApiFunction::Unregister,
                        failure: ExcelApiFailure::Status(ExcelCallbackStatus::Uncalced),
                    },
                ));
                outcome
            },
        );

        assert_eq!(result.journal.pending_registrations.len(), 1);
        assert_eq!(
            result.journal.pending_registrations[0].registration,
            registration
        );
    }

    #[test]
    fn failed_async_event_rollback_returns_cleanup_debt() {
        let attempts = std::cell::Cell::new(0);
        let mut callbacks = HostCallbackSession::new();
        let result = register_async_events_transaction(
            &mut callbacks,
            |_callbacks, procedure, event| {
                let attempt = attempts.get() + 1;
                attempts.set(attempt);
                if attempt == 2 {
                    Err(RegistrationTransactionError::new(XllError::ExcelApi {
                        function: ExcelApiFunction::EventRegister,
                        failure: ExcelApiFailure::Status(ExcelCallbackStatus::Failed(32)),
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
            |_callbacks, registrations| {
                let mut outcome = UnregisterResult::new(registrations.len());
                outcome.failed.push((
                    registrations[0].clone(),
                    XllError::ExcelApi {
                        function: ExcelApiFunction::EventRegister,
                        failure: ExcelApiFailure::Status(ExcelCallbackStatus::Uncalced),
                    },
                ));
                outcome
            },
        )
        .unwrap_err();

        assert_eq!(result.journal.pending_events.len(), 1);
        assert_eq!(result.journal.pending_events[0].registration_id, 1);
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
            EventUnregisterAttempt {
                status: ExcelCallbackStatus::Success,
                detached: Ok(()),
                release: Some(Err(XllError::ExcelApi {
                    function: ExcelApiFunction::Free,
                    failure: ExcelApiFailure::Status(ExcelCallbackStatus::Failed(32)),
                })),
            }
        });
        assert!(first.failed.is_empty());
        assert_eq!(first.cleanup_issues.len(), 1);
        assert!(first.succeeded[0].unregistered);

        let retry = unregister_events_with(
            &[first.succeeded[0].clone()],
            |_| -> EventUnregisterAttempt {
                calls.set(calls.get() + 1);
                EventUnregisterAttempt {
                    status: ExcelCallbackStatus::Success,
                    detached: Ok(()),
                    release: Some(Ok(())),
                }
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
        let result = unregister_events_with(&[registration], |_| EventUnregisterAttempt {
            status: ExcelCallbackStatus::Success,
            detached: Err(XllError::ExcelApi {
                function: ExcelApiFunction::EventRegister,
                failure: ExcelApiFailure::InvalidRegistrationId(0),
            }),
            release: Some(Ok(())),
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
        let result = unregister_events_with(&[registration], |_| EventUnregisterAttempt {
            status: ExcelCallbackStatus::Success,
            detached: Err(XllError::ExcelApi {
                function: ExcelApiFunction::EventRegister,
                failure: ExcelApiFailure::UnexpectedResult,
            }),
            release: Some(Ok(())),
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
        let mut callbacks = HostCallbackSession::new();
        let error = event_release_failure(
            &mut callbacks,
            registration.clone(),
            XllError::ExcelApi {
                function: ExcelApiFunction::EventRegister,
                failure: ExcelApiFailure::InvalidRegistrationId(0),
            },
            |_callbacks, registrations| {
                rollback_calls.set(rollback_calls.get() + 1);
                let mut outcome = UnregisterResult::new(registrations.len());
                outcome.failed.push((
                    registrations[0].clone(),
                    XllError::ExcelApi {
                        function: ExcelApiFunction::EventRegister,
                        failure: ExcelApiFailure::Status(ExcelCallbackStatus::Failed(
                            xlfn_sys::XLRET_FAILED,
                        )),
                    },
                ));
                outcome
            },
        );

        assert_eq!(rollback_calls.get(), 1);
        assert_eq!(error.journal.pending_events, vec![registration]);
    }

    #[test]
    fn cleanup_severity_ordering() {
        assert!(CleanupSeverity::BestEffort < CleanupSeverity::UnloadUnsafe);
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
        let mut outcome = UnregisterResult::<PendingRegistration>::new(1);
        outcome.metadata_debt.push(MetadataDebt::new(
            registration.registration,
            XllError::ExcelApi {
                function: ExcelApiFunction::SetName,
                failure: ExcelApiFailure::UnexpectedResult,
            },
        ));
        assert!(outcome.failed.is_empty());
        assert_eq!(outcome.metadata_debt.len(), 1);
        assert_eq!(outcome.metadata_debt[0].excel_name(), "TEST_FUNC");
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
        assert_eq!(reg.cleanup_severity(), CleanupSeverity::BestEffort);

        reg.state = RegistrationCleanupState::NameDeleted;
        assert_eq!(reg.cleanup_severity(), CleanupSeverity::BestEffort);
    }
}
