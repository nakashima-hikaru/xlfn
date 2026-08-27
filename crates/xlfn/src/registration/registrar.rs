//! Registration transaction and recovery policy.
//!
//! Excel ABI construction and callback result handling live in
//! [`super::host::RegistrationHost`].  This module only decides how those
//! host observations affect the registration journal and rollback policy.

use super::host::{RegistrationHost, RegistrationMutation};
use super::preflight::{PreparedRegistration, PreparedRegistrationSet};
use super::{
    EventRegistration, PendingRegistration, RegistrationId, RegistrationTransactionError,
    UnknownRegistrationState, UnregisterResult,
};
use crate::XllError;
use std::path::PathBuf;

#[cfg(feature = "async")]
use super::host::{CALCULATION_CANCELED_EVENT, CALCULATION_ENDED_EVENT};

pub(crate) struct HostRegistrar {
    module_path: PathBuf,
    module_units: Vec<u16>,
}

impl HostRegistrar {
    pub(crate) fn connect(
        host: &RegistrationHost<'_>,
    ) -> Result<Self, RegistrationTransactionError> {
        let module_name = host
            .module_name()
            .map_err(RegistrationTransactionError::new)?;
        if !module_name.path.is_absolute() {
            return Err(RegistrationTransactionError::new(XllError::input(
                "module",
                crate::error::InputError::Malformed(
                    "xlGetName did not return an absolute module path",
                ),
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
        host: &RegistrationHost<'_>,
        prepared: &PreparedRegistrationSet,
    ) -> Result<Vec<RegistrationId>, RegistrationTransactionError> {
        register_all_transaction(
            host,
            prepared.as_slice(),
            |host, registration| self.register_one(host, registration),
            Self::unregister_pending,
        )
    }

    #[cfg(feature = "async")]
    pub(crate) fn register_async_events(
        &self,
        host: &RegistrationHost<'_>,
    ) -> Result<Vec<EventRegistration>, RegistrationTransactionError> {
        register_async_events_transaction(
            host,
            |host, procedure, event| self.register_event(host, procedure, event),
            Self::unregister_events_detailed,
        )
    }

    #[cfg(feature = "async")]
    fn register_event(
        &self,
        host: &RegistrationHost<'_>,
        procedure: &'static str,
        event: i32,
    ) -> Result<EventRegistration, RegistrationTransactionError> {
        match host.register_event(procedure, event) {
            RegistrationMutation::Applied { value, cleanup } => match cleanup {
                Ok(()) => Ok(value),
                Err(error) => Err(event_release_failure(
                    host,
                    value,
                    error,
                    Self::unregister_events_detailed,
                )),
            },
            RegistrationMutation::Rejected { error } => {
                Err(RegistrationTransactionError::new(error))
            }
            RegistrationMutation::Indeterminate { status, error } => {
                let registration = EventRegistration {
                    procedure,
                    event,
                    registration_id: 0,
                    unregistered: false,
                };
                let mut transaction_error = RegistrationTransactionError::new(error);
                if status.is_terminal() {
                    transaction_error.journal.pending_events.push(registration);
                    Err(transaction_error)
                } else {
                    Err(event_release_failure(
                        host,
                        registration,
                        *transaction_error.source,
                        Self::unregister_events_detailed,
                    ))
                }
            }
        }
    }

    fn register_one(
        &self,
        host: &RegistrationHost<'_>,
        descriptor: &PreparedRegistration,
    ) -> Result<RegistrationId, RegistrationTransactionError> {
        if host
            .is_registered_name(descriptor.excel_name)
            .map_err(RegistrationTransactionError::new)?
        {
            return Err(RegistrationTransactionError::new(
                XllError::RegistrationConflict {
                    name: descriptor.excel_name,
                },
            ));
        }

        match host.register(&self.module_units, descriptor) {
            RegistrationMutation::Applied { value, cleanup } => match cleanup {
                Ok(()) => Ok(value),
                Err(error) => Err(registration_release_failure(
                    host,
                    value,
                    error,
                    Self::unregister_pending,
                )),
            },
            RegistrationMutation::Rejected { error } => {
                Err(RegistrationTransactionError::new(error))
            }
            RegistrationMutation::Indeterminate { status, error } if status.is_terminal() => {
                let mut transaction_error = RegistrationTransactionError::new(error.clone());
                transaction_error
                    .journal
                    .mark_unknown(UnknownRegistrationState {
                        export_name: descriptor.export_name,
                        excel_name: descriptor.excel_name,
                        recovery_error: error,
                    });
                Err(transaction_error)
            }
            RegistrationMutation::Indeterminate { error, .. } => {
                Err(self.reconcile_malformed_registration_result(host, descriptor, error))
            }
        }
    }

    fn reconcile_malformed_registration_result(
        &self,
        host: &RegistrationHost<'_>,
        descriptor: &PreparedRegistration,
        source: XllError,
    ) -> RegistrationTransactionError {
        reconcile_malformed_registration_result_with(
            host,
            descriptor,
            source,
            |host, excel_name| {
                host.registration_id(excel_name)
                    .map_err(RegistrationTransactionError::new)
            },
            Self::unregister_pending,
        )
    }

    pub(crate) fn unregister_pending(
        host: &RegistrationHost<'_>,
        registrations: &[PendingRegistration],
    ) -> UnregisterResult<PendingRegistration> {
        let mut outcome = UnregisterResult::new(registrations.len());
        for registration in registrations.iter().rev() {
            let mut registration = registration.clone();
            if !host.permits_callbacks() {
                outcome.failed.push((registration, XllError::Closing));
                continue;
            }
            if registration.state == super::RegistrationCleanupState::NameDeleted {
                outcome.succeeded.push(registration);
                continue;
            }

            if registration.state == super::RegistrationCleanupState::Registered {
                match host.unregister_registration(registration.registration) {
                    RegistrationMutation::Applied { cleanup, .. } => {
                        registration.state = super::RegistrationCleanupState::Unregistered;
                        if let Err(error) = cleanup {
                            outcome.cleanup_issues.push(error);
                        }
                    }
                    RegistrationMutation::Rejected { error }
                    | RegistrationMutation::Indeterminate { error, .. } => {
                        outcome.failed.push((registration, error));
                        continue;
                    }
                }
            }

            if !host.permits_callbacks() {
                outcome.metadata_debt.push(super::MetadataDebt::new(
                    registration.registration,
                    XllError::Closing,
                ));
                continue;
            }

            match host.delete_name(registration.registration.excel_name) {
                RegistrationMutation::Applied { cleanup, .. } => {
                    registration.state = super::RegistrationCleanupState::NameDeleted;
                    if let Err(error) = cleanup {
                        outcome.cleanup_issues.push(error);
                    }
                    outcome.succeeded.push(registration);
                }
                RegistrationMutation::Rejected { error }
                | RegistrationMutation::Indeterminate { error, .. } => {
                    outcome
                        .metadata_debt
                        .push(super::MetadataDebt::new(registration.registration, error));
                }
            }
        }
        outcome
    }

    pub(crate) fn unregister_events_detailed(
        host: &RegistrationHost<'_>,
        registrations: &[EventRegistration],
    ) -> UnregisterResult<EventRegistration> {
        unregister_events_with(registrations, |registration| {
            host.unregister_event(registration.event)
        })
    }
}

fn reconcile_malformed_registration_result_with(
    host: &RegistrationHost<'_>,
    descriptor: &PreparedRegistration,
    source: XllError,
    recover: impl FnOnce(
        &RegistrationHost<'_>,
        &'static str,
    ) -> Result<Option<RegistrationId>, RegistrationTransactionError>,
    unregister: impl FnOnce(
        &RegistrationHost<'_>,
        &[PendingRegistration],
    ) -> UnregisterResult<PendingRegistration>,
) -> RegistrationTransactionError {
    match recover(host, descriptor.excel_name) {
        Ok(Some(registration)) => {
            registration_release_failure(host, registration, source, unregister)
        }
        Ok(None) => RegistrationTransactionError::new(source),
        Err(recovery_error) => {
            let mut error = RegistrationTransactionError::new(source);
            error.journal.mark_unknown(UnknownRegistrationState {
                export_name: descriptor.export_name,
                excel_name: descriptor.excel_name,
                recovery_error: *recovery_error.source,
            });
            error
        }
    }
}

fn unregister_events_with(
    registrations: &[EventRegistration],
    mut unregister: impl FnMut(&EventRegistration) -> RegistrationMutation<()>,
) -> UnregisterResult<EventRegistration> {
    let mut outcome = UnregisterResult::new(registrations.len());
    for registration in registrations.iter().rev() {
        let mut registration = registration.clone();
        if registration.unregistered {
            outcome.succeeded.push(registration);
            continue;
        }
        match unregister(&registration) {
            RegistrationMutation::Applied { cleanup, .. } => {
                registration.unregistered = true;
                if let Err(error) = cleanup {
                    outcome.cleanup_issues.push(error);
                }
                outcome.succeeded.push(registration);
            }
            RegistrationMutation::Rejected { error }
            | RegistrationMutation::Indeterminate { error, .. } => {
                outcome.failed.push((registration, error));
            }
        }
    }
    outcome
}

fn register_all_transaction<T>(
    host: &RegistrationHost<'_>,
    descriptors: &[T],
    mut register: impl FnMut(
        &RegistrationHost<'_>,
        &T,
    ) -> Result<RegistrationId, RegistrationTransactionError>,
    mut unregister: impl FnMut(
        &RegistrationHost<'_>,
        &[PendingRegistration],
    ) -> UnregisterResult<PendingRegistration>,
) -> Result<Vec<RegistrationId>, RegistrationTransactionError> {
    let mut registered = Vec::with_capacity(descriptors.len());
    for descriptor in descriptors {
        match register(host, descriptor) {
            Ok(id) => registered.push(id),
            Err(mut error) => {
                let pending: Vec<_> = registered
                    .iter()
                    .copied()
                    .map(PendingRegistration::from)
                    .collect();
                if !host.permits_callbacks() {
                    error.journal.pending_registrations.extend(pending);
                } else {
                    let outcome = unregister(host, &pending);
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

fn registration_release_failure(
    host: &RegistrationHost<'_>,
    registration: RegistrationId,
    source: XllError,
    unregister: impl FnOnce(
        &RegistrationHost<'_>,
        &[PendingRegistration],
    ) -> UnregisterResult<PendingRegistration>,
) -> RegistrationTransactionError {
    let pending = [PendingRegistration::from(registration)];
    let mut error = RegistrationTransactionError::new(source);
    if !host.permits_callbacks() {
        error
            .journal
            .pending_registrations
            .extend_from_slice(&pending);
        return error;
    }
    let outcome = unregister(host, &pending);
    error.journal.pending_registrations =
        outcome.failed.into_iter().map(|(entry, _)| entry).collect();
    error.journal.metadata_debt = outcome.metadata_debt;
    error
}

#[cfg(feature = "async")]
fn event_release_failure(
    host: &RegistrationHost<'_>,
    registration: EventRegistration,
    source: XllError,
    unregister: impl FnOnce(
        &RegistrationHost<'_>,
        &[EventRegistration],
    ) -> UnregisterResult<EventRegistration>,
) -> RegistrationTransactionError {
    let mut error = RegistrationTransactionError::new(source);
    if !host.permits_callbacks() {
        error.journal.pending_events.push(registration);
        return error;
    }
    error.journal.pending_events = unregister(host, &[registration])
        .failed
        .into_iter()
        .map(|(entry, _)| entry)
        .collect();
    error
}

#[cfg(feature = "async")]
fn register_async_events_transaction(
    host: &RegistrationHost<'_>,
    mut register: impl FnMut(
        &RegistrationHost<'_>,
        &'static str,
        i32,
    ) -> Result<EventRegistration, RegistrationTransactionError>,
    mut unregister: impl FnMut(
        &RegistrationHost<'_>,
        &[EventRegistration],
    ) -> UnregisterResult<EventRegistration>,
) -> Result<Vec<EventRegistration>, RegistrationTransactionError> {
    let mut registrations = Vec::with_capacity(2);
    registrations.push(register(
        host,
        "__xlfn_calculation_canceled",
        CALCULATION_CANCELED_EVENT,
    )?);
    match register(host, "__xlfn_calculation_ended", CALCULATION_ENDED_EVENT) {
        Ok(registration) => {
            registrations.push(registration);
            Ok(registrations)
        }
        Err(mut error) => {
            if !host.permits_callbacks() {
                error.journal.pending_events.extend(registrations);
            } else {
                error.journal.pending_events.extend(
                    unregister(host, &registrations)
                        .failed
                        .into_iter()
                        .map(|(entry, _)| entry),
                );
            }
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_callback::HostCallbackSession;
    use crate::registration::RegistrationCleanupState;
    use crate::registration::ledger::CleanupSeverity;
    use crate::registration::preflight::preflight_registration;
    use crate::registration::schema::{ArgumentAbi, RegistrationDescriptor, RegistrationSignature};
    use crate::return_value::ExcelCallbackStatus;
    use std::cell::{Cell, RefCell};
    use xlfn_common::{ExecutionKind, FunctionVisibility};

    fn prepared_set() -> PreparedRegistrationSet {
        const ARGUMENTS: &[super::super::ArgumentDescriptor] = &[];
        const ABI_ARGUMENTS: &[ArgumentAbi] = &[];
        preflight_registration(&[RegistrationDescriptor {
            export_name: "test_export",
            excel_name: "TEST.EXPORT",
            signature: RegistrationSignature {
                execution: ExecutionKind::MainThread,
                arguments: ABI_ARGUMENTS,
                volatile: false,
            },
            category: "Test",
            description: "test",
            help_topic: "",
            visibility: FunctionVisibility::Public,
            arguments: ARGUMENTS,
        }])
        .unwrap()
    }

    #[test]
    fn preflight_plan_contains_the_encoded_host_arguments() {
        let prepared = prepared_set();
        let registration = &prepared.as_slice()[0];
        assert_eq!(registration.export_name_text.as_str(), "test_export");
        assert_eq!(registration.excel_name_text.as_str(), "TEST.EXPORT");
        assert_eq!(registration.argument_names.as_str(), "");
        assert_eq!(registration.type_text.as_str(), "Q");
    }

    #[test]
    fn register_all_rolls_back_already_applied_mutations() {
        let callbacks = HostCallbackSession::new();
        let host = RegistrationHost::new(&callbacks);
        let descriptors = ["FIRST", "SECOND", "THIRD"];
        let attempts = Cell::new(0);
        let rolled_back = RefCell::new(Vec::new());

        let error = register_all_transaction(
            &host,
            &descriptors,
            |_host, descriptor| {
                let attempt = attempts.get() + 1;
                attempts.set(attempt);
                if attempt == 3 {
                    Err(RegistrationTransactionError::new(XllError::Closing))
                } else {
                    Ok(RegistrationId {
                        id: attempt as f64,
                        excel_name: descriptor,
                    })
                }
            },
            |_host, registrations| {
                rolled_back
                    .borrow_mut()
                    .extend(registrations.iter().map(|entry| entry.registration.id));
                let mut result = UnregisterResult::new(registrations.len());
                result.succeeded.extend_from_slice(registrations);
                result
            },
        )
        .unwrap_err();

        assert_eq!(rolled_back.borrow().as_slice(), &[1.0, 2.0]);
        assert!(error.journal.pending_registrations.is_empty());
        assert!(error.journal.metadata_debt.is_empty());
    }

    #[test]
    fn terminal_registration_failure_preserves_prior_work_without_rollback() {
        let callbacks = HostCallbackSession::new();
        let host = RegistrationHost::new(&callbacks);
        let attempts = Cell::new(0);
        let rollback_calls = Cell::new(0);
        let descriptors = ["FIRST", "SECOND"];

        let error = register_all_transaction(
            &host,
            &descriptors,
            |_host, descriptor| {
                let attempt = attempts.get() + 1;
                attempts.set(attempt);
                if attempt == 2 {
                    callbacks.suppress_for_test(ExcelCallbackStatus::Abort);
                    let mut error = RegistrationTransactionError::new(XllError::ExcelApi {
                        function: crate::error::ExcelApiFunction::Register,
                        failure: crate::error::ExcelApiFailure::Status(ExcelCallbackStatus::Abort),
                    });
                    error.journal.mark_unknown(UnknownRegistrationState {
                        export_name: "second_export",
                        excel_name: descriptor,
                        recovery_error: XllError::Closing,
                    });
                    Err(error)
                } else {
                    Ok(RegistrationId {
                        id: attempt as f64,
                        excel_name: descriptor,
                    })
                }
            },
            |_host, _registrations| {
                rollback_calls.set(rollback_calls.get() + 1);
                UnregisterResult::new(0)
            },
        )
        .unwrap_err();

        assert_eq!(rollback_calls.get(), 0);
        assert_eq!(error.journal.pending_registrations.len(), 1);
        assert_eq!(error.journal.pending_registrations[0].registration.id, 1.0);
        assert_eq!(error.journal.unknown_registrations.len(), 1);
        assert!(!host.permits_callbacks());
    }

    #[test]
    fn registration_release_failure_retains_failed_cleanup() {
        let callbacks = HostCallbackSession::new();
        let host = RegistrationHost::new(&callbacks);
        let registration = RegistrationId {
            id: 7.0,
            excel_name: "RELEASE_FAILURE",
        };

        let result = registration_release_failure(
            &host,
            registration,
            XllError::Closing,
            |_host, registrations| {
                let mut result = UnregisterResult::new(registrations.len());
                result.failed.push((
                    registrations[0].clone(),
                    XllError::ExcelApi {
                        function: crate::error::ExcelApiFunction::Unregister,
                        failure: crate::error::ExcelApiFailure::Status(
                            ExcelCallbackStatus::Uncalced,
                        ),
                    },
                ));
                result
            },
        );

        assert_eq!(result.journal.pending_registrations.len(), 1);
        assert_eq!(
            result.journal.pending_registrations[0].registration,
            registration
        );
    }

    #[test]
    fn malformed_registration_recovery_marks_unknown_when_recovery_fails() {
        let prepared = prepared_set();
        let descriptor = &prepared.as_slice()[0];
        let callbacks = HostCallbackSession::new();
        let host = RegistrationHost::new(&callbacks);

        let error = reconcile_malformed_registration_result_with(
            &host,
            descriptor,
            XllError::ExcelApi {
                function: crate::error::ExcelApiFunction::Register,
                failure: crate::error::ExcelApiFailure::UnexpectedResult,
            },
            |_host, _name| Err(RegistrationTransactionError::new(XllError::Closing)),
            |_host, _registrations| panic!("unknown registration must not be unregistered"),
        );

        assert_eq!(error.journal.unknown_registrations.len(), 1);
        assert_eq!(
            error.journal.unknown_registrations[0].excel_name,
            "TEST.EXPORT"
        );
        assert!(error.journal.is_unknown());
    }

    #[test]
    fn malformed_registration_recovery_rolls_back_when_binding_is_found() {
        let prepared = prepared_set();
        let descriptor = &prepared.as_slice()[0];
        let callbacks = HostCallbackSession::new();
        let host = RegistrationHost::new(&callbacks);
        let unregistered = Cell::new(None);

        let error = reconcile_malformed_registration_result_with(
            &host,
            descriptor,
            XllError::Closing,
            |_host, excel_name| {
                Ok(Some(RegistrationId {
                    id: 42.0,
                    excel_name,
                }))
            },
            |_host, registrations| {
                unregistered.set(Some(registrations[0].registration.id));
                let mut result = UnregisterResult::new(registrations.len());
                result.succeeded.extend_from_slice(registrations);
                result
            },
        );

        assert_eq!(unregistered.get(), Some(42.0));
        assert!(error.journal.pending_registrations.is_empty());
        assert!(!error.journal.is_unknown());
    }

    #[test]
    fn unregister_mutation_is_committed_before_result_cleanup() {
        let registration = EventRegistration {
            procedure: "event",
            event: 1,
            registration_id: 1,
            unregistered: false,
        };
        let first = unregister_events_with(&[registration], |_registration| {
            RegistrationMutation::Applied {
                value: (),
                cleanup: Err(XllError::Closing),
            }
        });

        assert!(first.failed.is_empty());
        assert_eq!(first.cleanup_issues.len(), 1);
        assert!(first.succeeded[0].unregistered);

        let retry = unregister_events_with(&first.succeeded, |_registration| {
            panic!("an applied unregister must not be repeated")
        });
        assert_eq!(retry.succeeded.len(), 1);
    }

    #[test]
    fn pending_registration_cleanup_severity_matches_state() {
        let mut registration = PendingRegistration::from(RegistrationId {
            id: 1.0,
            excel_name: "FOO",
        });
        assert_eq!(
            registration.cleanup_severity(),
            CleanupSeverity::UnloadUnsafe
        );

        registration.state = RegistrationCleanupState::Unregistered;
        assert_eq!(registration.cleanup_severity(), CleanupSeverity::BestEffort);
        registration.state = RegistrationCleanupState::NameDeleted;
        assert_eq!(registration.cleanup_severity(), CleanupSeverity::BestEffort);
    }

    #[cfg(feature = "async")]
    #[test]
    fn second_async_event_failure_rolls_back_the_first_event() {
        let callbacks = HostCallbackSession::new();
        let host = RegistrationHost::new(&callbacks);
        let attempts = Cell::new(0);
        let rolled_back = RefCell::new(Vec::new());

        let result = register_async_events_transaction(
            &host,
            |_host, procedure, event| {
                let attempt = attempts.get() + 1;
                attempts.set(attempt);
                if attempt == 2 {
                    Err(RegistrationTransactionError::new(XllError::Closing))
                } else {
                    Ok(EventRegistration {
                        procedure,
                        event,
                        registration_id: attempt,
                        unregistered: false,
                    })
                }
            },
            |_host, registrations| {
                rolled_back.borrow_mut().extend_from_slice(registrations);
                let mut result = UnregisterResult::new(registrations.len());
                result.succeeded.extend_from_slice(registrations);
                result
            },
        )
        .unwrap_err();

        assert_eq!(rolled_back.borrow().len(), 1);
        assert_eq!(rolled_back.borrow()[0].registration_id, 1);
        assert!(result.journal.pending_events.is_empty());
    }
}
