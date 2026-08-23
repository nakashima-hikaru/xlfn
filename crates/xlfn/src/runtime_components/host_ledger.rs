//! Host registration and metadata-debt recovery ledger.

use parking_lot::Mutex;
use std::collections::BTreeMap;

use crate::registration::{
    EventRegistration, ExcelNameKey, HostMutationJournal, MetadataDebt, PendingRegistration,
    RegistrationCertainty,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RegistrationState {
    Known,
    Unknown,
}

struct HostLedgerState {
    registrations: Vec<PendingRegistration>,
    metadata_debt: BTreeMap<ExcelNameKey, Vec<MetadataDebt>>,
    event_registrations: Vec<EventRegistration>,
    registration_state: RegistrationState,
}

impl HostLedgerState {
    const fn new() -> Self {
        Self {
            registrations: Vec::new(),
            metadata_debt: BTreeMap::new(),
            event_registrations: Vec::new(),
            registration_state: RegistrationState::Known,
        }
    }
}

/// The Excel host registration protocol and its recovery ledger.
///
/// Registration mutations, event registrations, metadata debt, and the
/// registration-state certainty flag form one cold-path transaction domain.
/// Keeping them under one mutex makes the snapshot used by shutdown
/// certification coherent without maintaining several synchronization orders.
pub(crate) struct HostLedger {
    state: Mutex<HostLedgerState>,
}

impl HostLedger {
    pub(crate) const fn new() -> Self {
        Self {
            state: Mutex::new(HostLedgerState::new()),
        }
    }

    pub(crate) fn merge(&self, journal: HostMutationJournal) {
        let mut state = self.state.lock();
        state.registrations.extend(journal.pending_registrations);
        state.event_registrations.extend(journal.pending_events);
        for debt in journal.metadata_debt {
            state
                .metadata_debt
                .entry(debt.key())
                .or_default()
                .push(debt);
        }
        if journal.certainty == RegistrationCertainty::Unknown {
            state.registration_state = RegistrationState::Unknown;
        }
    }

    pub(crate) fn registrations_snapshot(&self) -> Vec<PendingRegistration> {
        self.state.lock().registrations.clone()
    }

    pub(crate) fn event_registrations_snapshot(&self) -> Vec<EventRegistration> {
        self.state.lock().event_registrations.clone()
    }

    pub(crate) fn callbacks_detached(&self) -> bool {
        let state = self.state.lock();
        state.registrations.is_empty() && state.event_registrations.is_empty()
    }

    pub(crate) fn is_quiescent(&self) -> bool {
        let state = self.state.lock();
        state.registrations.is_empty()
            && state.event_registrations.is_empty()
            && state.registration_state == RegistrationState::Known
    }

    pub(crate) fn replace_registrations(&self, registrations: Vec<PendingRegistration>) {
        self.state.lock().registrations = registrations;
    }

    pub(crate) fn replace_event_registrations(&self, registrations: Vec<EventRegistration>) {
        self.state.lock().event_registrations = registrations;
    }

    pub(crate) fn registration_state_unknown(&self) -> bool {
        self.state.lock().registration_state == RegistrationState::Unknown
    }

    pub(crate) fn retain_metadata_debt(&self, debts: Vec<MetadataDebt>) {
        let mut state = self.state.lock();
        for debt in debts {
            state
                .metadata_debt
                .entry(debt.key())
                .or_default()
                .push(debt);
        }
    }

    pub(crate) fn metadata_debt_snapshot(&self) -> BTreeMap<ExcelNameKey, Vec<MetadataDebt>> {
        self.state.lock().metadata_debt.clone()
    }

    pub(crate) fn clear_metadata_debt_for_registrations(
        &self,
        registrations: &[crate::registration::RegistrationId],
    ) {
        let mut state = self.state.lock();
        for registration in registrations {
            state
                .metadata_debt
                .remove(&ExcelNameKey::new(registration.excel_name));
        }
    }

    pub(crate) fn replace_metadata_debt(&self, debts: BTreeMap<ExcelNameKey, Vec<MetadataDebt>>) {
        self.state.lock().metadata_debt = debts;
    }

    pub(crate) fn has_metadata_debt(&self) -> bool {
        !self.state.lock().metadata_debt.is_empty()
    }
}
