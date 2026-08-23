//! Recovery ledger entries retained across callback failures.

use super::RegistrationId;
use crate::XllError;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PendingRegistration {
    pub(crate) registration: RegistrationId,
    pub(crate) state: RegistrationCleanupState,
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
pub(crate) struct ExcelNameKey(String);

impl ExcelNameKey {
    pub(crate) fn new(name: &str) -> Self {
        Self(name.to_ascii_uppercase())
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
    pub(crate) fn excel_name(&self) -> &'static str {
        self.registration.excel_name
    }

    #[cfg(test)]
    pub(crate) fn attempts(&self) -> u32 {
        self.attempts
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
pub enum CleanupSeverity {
    BestEffort,
    UnloadUnsafe,
}

impl CleanupSeverity {
    #[must_use]
    pub fn is_unload_unsafe(self) -> bool {
        matches!(self, Self::UnloadUnsafe)
    }
}

impl PendingRegistration {
    #[must_use]
    pub(crate) fn cleanup_severity(&self) -> CleanupSeverity {
        match self.state {
            RegistrationCleanupState::Registered => CleanupSeverity::UnloadUnsafe,
            RegistrationCleanupState::Unregistered => CleanupSeverity::BestEffort,
            RegistrationCleanupState::NameDeleted => CleanupSeverity::BestEffort,
        }
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RegistrationCertainty {
    Known,
    Unknown,
}

/// Host mutations that belong to one registration/open transaction.
///
/// The journal is the sole owner of cleanup obligations until the transaction
/// commits them into [`HostLedger`].  Keeping registrations, events, metadata
/// debt, and certainty together prevents one failure path from retaining only
/// part of the host-side side effects.
pub(crate) struct HostMutationJournal {
    pub(crate) pending_registrations: Vec<PendingRegistration>,
    pub(crate) pending_events: Vec<EventRegistration>,
    pub(crate) metadata_debt: Vec<MetadataDebt>,
    pub(crate) unknown_registrations: Vec<UnknownRegistrationState>,
    pub(crate) certainty: RegistrationCertainty,
}

impl Default for HostMutationJournal {
    fn default() -> Self {
        Self {
            pending_registrations: Vec::new(),
            pending_events: Vec::new(),
            metadata_debt: Vec::new(),
            unknown_registrations: Vec::new(),
            certainty: RegistrationCertainty::Known,
        }
    }
}

impl HostMutationJournal {
    pub(crate) fn mark_unknown(&mut self, unknown: UnknownRegistrationState) {
        self.certainty = RegistrationCertainty::Unknown;
        self.unknown_registrations.push(unknown);
    }

    pub(crate) fn merge(&mut self, mut other: Self) {
        self.pending_registrations
            .append(&mut other.pending_registrations);
        self.pending_events.append(&mut other.pending_events);
        self.metadata_debt.append(&mut other.metadata_debt);
        self.unknown_registrations
            .append(&mut other.unknown_registrations);
        if other.certainty == RegistrationCertainty::Unknown {
            self.certainty = RegistrationCertainty::Unknown;
        }
    }

    pub(crate) fn is_unknown(&self) -> bool {
        self.certainty == RegistrationCertainty::Unknown
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EventRegistration {
    pub(crate) procedure: &'static str,
    pub(crate) event: i32,
    pub(crate) registration_id: i32,
    pub(crate) unregistered: bool,
}
