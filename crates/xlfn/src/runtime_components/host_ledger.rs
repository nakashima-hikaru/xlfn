//! Host registration and metadata-debt recovery ledger.

use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::registration::{EventRegistration, ExcelNameKey, MetadataDebt, PendingRegistration};

/// The Excel host registration protocol and its recovery ledger.
pub(crate) struct HostLedger {
    pub(crate) registrations: Mutex<Vec<PendingRegistration>>,
    pub(crate) metadata_debt: Mutex<BTreeMap<ExcelNameKey, Vec<MetadataDebt>>>,
    pub(crate) event_registrations: Mutex<Vec<EventRegistration>>,
    pub(crate) registration_state_unknown: AtomicBool,
}

impl HostLedger {
    pub(crate) const fn new() -> Self {
        Self {
            registrations: Mutex::new(Vec::new()),
            metadata_debt: Mutex::new(BTreeMap::new()),
            event_registrations: Mutex::new(Vec::new()),
            registration_state_unknown: AtomicBool::new(false),
        }
    }

    pub(crate) fn append_registrations(
        &self,
        registrations: impl IntoIterator<Item = PendingRegistration>,
    ) {
        self.registrations.lock().extend(registrations);
    }

    pub(crate) fn append_event_registrations(
        &self,
        registrations: impl IntoIterator<Item = EventRegistration>,
    ) {
        self.event_registrations.lock().extend(registrations);
    }

    pub(crate) fn registrations_snapshot(&self) -> Vec<PendingRegistration> {
        self.registrations.lock().clone()
    }

    pub(crate) fn event_registrations_snapshot(&self) -> Vec<EventRegistration> {
        self.event_registrations.lock().clone()
    }

    pub(crate) fn registrations_empty(&self) -> bool {
        self.registrations.lock().is_empty()
    }

    pub(crate) fn event_registrations_empty(&self) -> bool {
        self.event_registrations.lock().is_empty()
    }

    pub(crate) fn replace_registrations(&self, registrations: Vec<PendingRegistration>) {
        *self.registrations.lock() = registrations;
    }

    pub(crate) fn replace_event_registrations(&self, registrations: Vec<EventRegistration>) {
        *self.event_registrations.lock() = registrations;
    }

    pub(crate) fn mark_registration_state_unknown(&self) {
        self.registration_state_unknown
            .store(true, Ordering::Release);
    }

    pub(crate) fn registration_state_unknown(&self) -> bool {
        self.registration_state_unknown.load(Ordering::Acquire)
    }

    pub(crate) fn retain_metadata_debt(&self, debts: Vec<MetadataDebt>) {
        let mut retained = self.metadata_debt.lock();
        for debt in debts {
            retained.entry(debt.key()).or_default().push(debt);
        }
    }

    pub(crate) fn metadata_debt_snapshot(&self) -> BTreeMap<ExcelNameKey, Vec<MetadataDebt>> {
        self.metadata_debt.lock().clone()
    }

    pub(crate) fn clear_metadata_debt_for_registrations(
        &self,
        registrations: &[crate::registration::RegistrationId],
    ) {
        let mut debts = self.metadata_debt.lock();
        for registration in registrations {
            debts.remove(&ExcelNameKey::new(registration.excel_name));
        }
    }

    pub(crate) fn replace_metadata_debt(&self, debts: BTreeMap<ExcelNameKey, Vec<MetadataDebt>>) {
        *self.metadata_debt.lock() = debts;
    }

    pub(crate) fn has_metadata_debt(&self) -> bool {
        !self.metadata_debt.lock().is_empty()
    }
}
