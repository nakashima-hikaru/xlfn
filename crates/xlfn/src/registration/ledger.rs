//! Recovery ledger entries retained across callback failures.

use super::RegistrationId;
use crate::XllError;
use smol_str::{SmolStr, StrExt};
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Debug, PartialEq)]
pub(crate) struct PendingRegistration {
    registration: RegistrationId,
    state: RegistrationCleanupState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RegistrationCleanupState {
    Registered,
    Unregistered,
    NameDeleted,
}

/// The single host-name identity rule used by descriptor validation and
/// cleanup debt retention.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct ExcelNameKey(SmolStr);

impl ExcelNameKey {
    pub(crate) fn new(name: &str) -> Self {
        if name.len() <= 23 {
            return Self(name.to_ascii_uppercase_smolstr());
        }
        // The library's long-string helper first allocates a String, then
        // copies it into an Arc. Normalize the fresh Arc in place instead.
        let mut normalized: Arc<str> = Arc::from(name);
        // Fresh ownership gives make_mut the existing allocation. Its safe
        // copy-on-write result also keeps exclusivity if construction changes.
        Arc::make_mut(&mut normalized).make_ascii_uppercase();
        Self(SmolStr::from(normalized))
    }
}

#[derive(Clone, Debug)]
pub(crate) struct MetadataDebt {
    pub(crate) registration: RegistrationId,
    attempts: u32,
    last_error: XllError,
}

impl MetadataDebt {
    pub(crate) fn new(registration: RegistrationId, error: XllError) -> Self {
        Self {
            registration,
            attempts: 1,
            last_error: error,
        }
    }

    pub(crate) fn retry_failed(&self, error: XllError) -> Self {
        Self {
            registration: self.registration,
            attempts: self.attempts.saturating_add(1),
            last_error: error,
        }
    }

    #[cfg(test)]
    pub(crate) fn expected_registration_id(&self) -> f64 {
        self.registration.id
    }

    pub(crate) fn last_error(&self) -> &XllError {
        &self.last_error
    }

    pub(crate) fn key(&self) -> ExcelNameKey {
        ExcelNameKey::new(self.registration.excel_name)
    }
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
pub(crate) enum CleanupSeverity {
    BestEffort,
    UnloadUnsafe,
}

impl CleanupSeverity {
    #[must_use]
    pub(crate) fn is_unload_unsafe(self) -> bool {
        matches!(self, Self::UnloadUnsafe)
    }
}

pub(crate) struct UnregisterResult<T> {
    pub(crate) succeeded: Vec<T>,
    pub(crate) failed: Vec<(T, XllError)>,
    pub(crate) metadata_debt: Vec<MetadataDebt>,
    pub(crate) cleanup_issues: Vec<XllError>,
}

#[derive(Clone, Debug)]
pub(crate) struct UnknownRegistrationState {
    pub(crate) export_name: &'static str,
    pub(crate) excel_name: &'static str,
    pub(crate) recovery_error: XllError,
}

/// Host mutations that belong to one registration/open transaction.
///
/// The journal is the sole owner of cleanup obligations until the transaction
/// commits them into [`HostLedger`].  Keeping registrations, metadata
/// debt, and certainty together prevents one failure path from retaining only
/// part of the host-side side effects.
#[derive(Default)]
pub(crate) struct HostMutationJournal {
    pending_registrations: Vec<PendingRegistration>,
    metadata_debt: Vec<MetadataDebt>,
    unknown_registrations: Vec<UnknownRegistrationState>,
    superseded_metadata: Vec<ExcelNameKey>,
}

impl HostMutationJournal {
    pub(crate) fn from_registrations(
        registrations: impl IntoIterator<Item = RegistrationId>,
    ) -> Self {
        let mut journal = Self::default();
        journal.record_registrations(registrations);
        journal
    }
    pub(crate) fn record_registrations(
        &mut self,
        registrations: impl IntoIterator<Item = RegistrationId>,
    ) {
        self.pending_registrations
            .extend(registrations.into_iter().map(PendingRegistration::from));
    }
    pub(super) fn retain_pending(
        &mut self,
        registrations: impl IntoIterator<Item = PendingRegistration>,
    ) {
        self.pending_registrations.extend(registrations);
    }
    pub(super) fn retain_metadata(&mut self, debts: Vec<MetadataDebt>) {
        self.metadata_debt.extend(debts);
    }
    #[cfg(test)]
    pub(crate) fn pending_registrations(&self) -> &[PendingRegistration] {
        &self.pending_registrations
    }
    #[cfg(test)]
    pub(crate) fn metadata_debt(&self) -> &[MetadataDebt] {
        &self.metadata_debt
    }
    pub(crate) fn unknown_registrations(&self) -> &[UnknownRegistrationState] {
        &self.unknown_registrations
    }
    pub(crate) fn into_parts(
        self,
    ) -> (
        Vec<PendingRegistration>,
        Vec<MetadataDebt>,
        bool,
        Vec<ExcelNameKey>,
    ) {
        let unknown = self.is_unknown();
        (
            self.pending_registrations,
            self.metadata_debt,
            unknown,
            self.superseded_metadata,
        )
    }
    pub(crate) fn mark_unknown(&mut self, unknown: UnknownRegistrationState) {
        self.unknown_registrations.push(unknown);
    }
    pub(crate) fn merge(&mut self, mut other: Self) {
        self.pending_registrations
            .append(&mut other.pending_registrations);
        self.metadata_debt.append(&mut other.metadata_debt);
        self.unknown_registrations
            .append(&mut other.unknown_registrations);
        self.superseded_metadata
            .append(&mut other.superseded_metadata);
    }
    pub(crate) fn is_unknown(&self) -> bool {
        !self.unknown_registrations.is_empty()
    }
}

pub(crate) struct RegistrationTransactionError {
    pub(crate) source: Box<XllError>,
    pub(crate) journal: HostMutationJournal,
}

impl RegistrationTransactionError {
    pub(crate) fn new(source: XllError) -> Self {
        Self {
            source: Box::new(source),
            journal: HostMutationJournal::default(),
        }
    }
}

impl<T> UnregisterResult<T> {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            succeeded: Vec::with_capacity(capacity),
            failed: Vec::new(),
            metadata_debt: Vec::new(),
            cleanup_issues: Vec::new(),
        }
    }
}

pub(crate) struct MetadataDebtRetryResult {
    pub(crate) remaining: BTreeMap<ExcelNameKey, Vec<MetadataDebt>>,
    pub(crate) cleanup_issues: Vec<XllError>,
    pub(crate) terminal: Option<XllError>,
}

#[cfg(test)]
mod tests {
    use super::ExcelNameKey;
    use std::collections::{BTreeMap, HashMap};

    #[test]
    fn excel_name_identity_stays_ascii_only_across_storage_sizes() {
        for name in [
            String::new(),
            "price".into(),
            "a".repeat(23),
            "a".repeat(24),
            "long_name".repeat(128),
            "école価格".into(),
            "école価格".repeat(16),
        ] {
            let expected = name.to_ascii_uppercase();
            let key = ExcelNameKey::new(&name);
            assert_eq!(key.0.as_str(), expected);
            assert_eq!(key, ExcelNameKey::new(&expected));
            let ordered = BTreeMap::from([(key.clone(), 1)]);
            let hashed = HashMap::from([(key, 1)]);
            assert_eq!(ordered.get(&ExcelNameKey::new(&expected)), Some(&1));
            assert_eq!(hashed.get(&ExcelNameKey::new(&expected)), Some(&1));
        }
        assert_ne!(ExcelNameKey::new("é"), ExcelNameKey::new("É"));
    }
}

/// Observation only: cloning this value does not duplicate cleanup authority.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CleanupSnapshot {
    pub(crate) registration: RegistrationId,
    pub(crate) state: RegistrationCleanupState,
}
impl CleanupSnapshot {
    pub(crate) fn cleanup_severity(&self) -> CleanupSeverity {
        match self.state {
            RegistrationCleanupState::Registered => CleanupSeverity::UnloadUnsafe,
            _ => CleanupSeverity::BestEffort,
        }
    }
}
impl PendingRegistration {
    pub(crate) fn registration(&self) -> RegistrationId {
        self.registration
    }
    pub(crate) fn snapshot(&self) -> CleanupSnapshot {
        CleanupSnapshot {
            registration: self.registration(),
            state: self.state,
        }
    }
}

use super::host::{RegistrationHost, RegistrationMutation};
use super::registrar::HostRegistrar;
impl HostRegistrar {
    pub(crate) fn unregister_pending(
        host: &RegistrationHost<'_>,
        registrations: Vec<PendingRegistration>,
    ) -> UnregisterResult<PendingRegistration> {
        let mut outcome = UnregisterResult::new(registrations.len());
        for mut registration in registrations.into_iter().rev() {
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

            match host.delete_name_if_binding_matches(
                registration.registration.excel_name,
                [registration.registration.id],
            ) {
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
}

#[cfg(test)]
mod phase_tests {
    use super::*;
    #[test]
    fn pending_registration_cleanup_severity_matches_state() {
        let mut registration = PendingRegistration::from(RegistrationId {
            id: 1.0,
            excel_name: "FOO",
        });
        assert_eq!(
            registration.snapshot().cleanup_severity(),
            CleanupSeverity::UnloadUnsafe
        );

        registration.state = RegistrationCleanupState::Unregistered;
        assert_eq!(
            registration.snapshot().cleanup_severity(),
            CleanupSeverity::BestEffort
        );
        registration.state = RegistrationCleanupState::NameDeleted;
        assert_eq!(
            registration.snapshot().cleanup_severity(),
            CleanupSeverity::BestEffort
        );
    }

    #[test]
    fn only_success_receipt_can_supersede_metadata_debt() {
        use crate::runtime_components::HostLedger;
        let debt = || {
            MetadataDebt::new(
                RegistrationId {
                    id: 1.0,
                    excel_name: "price",
                },
                XllError::Closing,
            )
        };
        let id = RegistrationId {
            id: 2.0,
            excel_name: "PRICE",
        };
        let observed_only = HostLedger::new();
        observed_only.retain_metadata_debt(vec![debt()]);
        observed_only.merge(HostMutationJournal::from_registrations([id]));
        assert!(
            observed_only.has_metadata_debt(),
            "scalar IDs carry no success receipt"
        );

        let applied = HostLedger::new();
        applied.retain_metadata_debt(vec![debt()]);
        applied.merge(SuccessfulRegistrations::from_applied(vec![id]).into_journal());
        assert!(!applied.has_metadata_debt());
        assert_eq!(applied.registrations_snapshot().len(), 1);
    }
}

/// Only the successful host registration path constructs this receipt.
/// It owns the same Vec as the previous scalar-ID return, with no marker cost.
pub(crate) struct SuccessfulRegistrations(Vec<RegistrationId>);
impl SuccessfulRegistrations {
    pub(super) fn from_applied(registrations: Vec<RegistrationId>) -> Self {
        Self(registrations)
    }
    pub(crate) fn into_journal(self) -> HostMutationJournal {
        let superseded_metadata = self
            .0
            .iter()
            .map(|id| ExcelNameKey::new(id.excel_name))
            .collect();
        let mut journal = HostMutationJournal::from_registrations(self.0);
        journal.superseded_metadata = superseded_metadata;
        journal
    }
}
impl std::ops::Deref for SuccessfulRegistrations {
    type Target = [RegistrationId];
    fn deref(&self) -> &[RegistrationId] {
        &self.0
    }
}
