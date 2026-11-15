//! Host registration and metadata-debt recovery ledger.

use crate::sync::Mutex;
use std::collections::BTreeMap;
use xlfn_kernel::rollback::{Rollback, RollbackGuard};

use crate::registration::{ExcelNameKey, HostMutationJournal, MetadataDebt, PendingRegistration};

type MetadataDebts = BTreeMap<ExcelNameKey, Vec<MetadataDebt>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RegistrationState {
    Known,
    Unknown,
}

struct HostLedgerState {
    registrations: Vec<PendingRegistration>,
    metadata_debt: MetadataDebts,
    registration_state: RegistrationState,
    cleanup_in_progress: bool,
    metadata_cleanup_in_progress: bool,
}

impl HostLedgerState {
    const fn new() -> Self {
        Self {
            registrations: Vec::new(),
            metadata_debt: BTreeMap::new(),
            registration_state: RegistrationState::Known,
            cleanup_in_progress: false,
            metadata_cleanup_in_progress: false,
        }
    }

    fn append_metadata_debts(&mut self, debts: MetadataDebts) {
        for (key, mut debts) in debts {
            self.metadata_debt
                .entry(key)
                .or_default()
                .append(&mut debts);
        }
    }
}

/// The Excel host registration protocol and its recovery ledger.
///
/// Generation function registrations, metadata debt, and the
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
        let (registrations, metadata, unknown, superseded) = journal.into_parts();
        let mut state = self.state.lock();
        for key in superseded {
            state.metadata_debt.remove(&key);
        }
        state.registrations.extend(registrations);
        for debt in metadata {
            state
                .metadata_debt
                .entry(debt.key())
                .or_default()
                .push(debt);
        }
        if unknown {
            state.registration_state = RegistrationState::Unknown;
        }
    }

    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn registrations_snapshot(
        &self,
    ) -> Vec<crate::registration::ledger::CleanupSnapshot> {
        self.state
            .lock()
            .registrations
            .iter()
            .map(PendingRegistration::snapshot)
            .collect()
    }

    /// One coherent observation: a moved-out cleanup batch must never look
    /// both empty and known to the shutdown resource projection.
    #[cfg(any(test, feature = "refinement"))]
    pub(crate) fn registration_resources(&self) -> (usize, bool) {
        let state = self.state.lock();
        (
            state.registrations.len(),
            !state.cleanup_in_progress
                && !state.metadata_cleanup_in_progress
                && state.registration_state == RegistrationState::Known,
        )
    }

    pub(crate) fn generation_callbacks_detached(&self) -> bool {
        let state = self.state.lock();
        !state.cleanup_in_progress
            && !state.metadata_cleanup_in_progress
            && state.registrations.is_empty()
    }

    pub(crate) fn is_quiescent(&self) -> bool {
        let state = self.state.lock();
        !state.cleanup_in_progress
            && !state.metadata_cleanup_in_progress
            && state.registrations.is_empty()
            && state.registration_state == RegistrationState::Known
    }

    pub(crate) fn cleanup_registrations(
        &self,
        host: &crate::registration::RegistrationHost<'_>,
    ) -> crate::XllResult<RegistrationCleanupReport> {
        self.claim_cleanup()?.execute(host)
    }

    fn claim_cleanup(&self) -> crate::XllResult<PreparedCleanup<'_, Vec<PendingRegistration>>> {
        let mut state = self.state.lock();
        if state.cleanup_in_progress {
            return Err(crate::XllError::Closing);
        }
        state.cleanup_in_progress = true;
        let registrations = std::mem::take(&mut state.registrations);
        Ok(PreparedCleanup::new(self, registrations))
    }

    pub(crate) fn registration_state_unknown(&self) -> bool {
        let state = self.state.lock();
        state.cleanup_in_progress
            || state.metadata_cleanup_in_progress
            || state.registration_state == RegistrationState::Unknown
    }

    #[cfg(test)]
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

    #[cfg(test)]
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

    pub(crate) fn retry_metadata_cleanup(
        &self,
        host: &crate::registration::RegistrationHost<'_>,
    ) -> crate::registration::MetadataDebtRetryResult {
        match self.claim_metadata_cleanup() {
            Ok(claim) => claim.execute(host),
            Err(error) => crate::registration::MetadataDebtRetryResult {
                remaining: BTreeMap::new(),
                cleanup_issues: Vec::new(),
                terminal: Some(error),
            },
        }
    }

    fn claim_metadata_cleanup(&self) -> crate::XllResult<PreparedCleanup<'_, MetadataDebts>> {
        let mut state = self.state.lock();
        if state.metadata_cleanup_in_progress {
            return Err(crate::XllError::Closing);
        }
        state.metadata_cleanup_in_progress = true;
        let debts = std::mem::take(&mut state.metadata_debt);
        Ok(PreparedCleanup::new(self, debts))
    }

    pub(crate) fn has_metadata_debt(&self) -> bool {
        let state = self.state.lock();
        state.metadata_cleanup_in_progress || !state.metadata_debt.is_empty()
    }
}

/// Cloneable observations, never executable registration cleanup authority.
pub(crate) struct RegistrationCleanupReport {
    pub(crate) succeeded: Vec<crate::registration::ledger::CleanupSnapshot>,
    pub(crate) failed: Vec<(
        crate::registration::ledger::CleanupSnapshot,
        crate::XllError,
    )>,
    pub(crate) metadata_debt: Vec<MetadataDebt>,
    pub(crate) cleanup_issues: Vec<crate::XllError>,
}

/// Holds the moved batch and the right to restore it before any host operation.
struct PreparedCleanup<'ledger, T>(RollbackGuard<PendingCleanup<'ledger, T>>)
where
    PendingCleanup<'ledger, T>: Rollback;

struct PendingCleanup<'ledger, T> {
    ledger: &'ledger HostLedger,
    entries: T,
}

/// Interruptions leave the gate set because host effects may be partial.
/// Only an observed outcome can publish the residual entries and open the gate.
struct ExecutingCleanup<'ledger, T> {
    ledger: &'ledger HostLedger,
    entries: T,
}

impl<'ledger, T> PreparedCleanup<'ledger, T>
where
    PendingCleanup<'ledger, T>: Rollback,
{
    fn new(ledger: &'ledger HostLedger, entries: T) -> Self {
        Self(RollbackGuard::new(PendingCleanup { ledger, entries }))
    }

    fn begin(self) -> ExecutingCleanup<'ledger, T> {
        let PendingCleanup { ledger, entries } = self.0.complete();
        ExecutingCleanup { ledger, entries }
    }
}

impl Rollback for PendingCleanup<'_, Vec<PendingRegistration>> {
    fn rollback(self) {
        let mut state = self.ledger.state.lock();
        state.registrations.extend(self.entries);
        state.cleanup_in_progress = false;
    }
}

impl PreparedCleanup<'_, Vec<PendingRegistration>> {
    fn execute(
        self,
        host: &crate::registration::RegistrationHost<'_>,
    ) -> crate::XllResult<RegistrationCleanupReport> {
        self.begin().execute(host)
    }
}

impl ExecutingCleanup<'_, Vec<PendingRegistration>> {
    fn execute(
        self,
        host: &crate::registration::RegistrationHost<'_>,
    ) -> crate::XllResult<RegistrationCleanupReport> {
        let outcome = crate::registration::HostRegistrar::unregister_pending(host, self.entries);
        let report = RegistrationCleanupReport {
            succeeded: outcome
                .succeeded
                .iter()
                .map(PendingRegistration::snapshot)
                .collect(),
            failed: outcome
                .failed
                .iter()
                .map(|(entry, error)| (entry.snapshot(), error.clone()))
                .collect(),
            metadata_debt: outcome.metadata_debt.clone(),
            cleanup_issues: outcome.cleanup_issues,
        };
        let mut state = self.ledger.state.lock();
        state
            .registrations
            .extend(outcome.failed.into_iter().map(|(entry, _)| entry));
        for debt in outcome.metadata_debt {
            state
                .metadata_debt
                .entry(debt.key())
                .or_default()
                .push(debt);
        }
        state.cleanup_in_progress = false;
        Ok(report)
    }
}

impl Rollback for PendingCleanup<'_, MetadataDebts> {
    fn rollback(self) {
        let mut state = self.ledger.state.lock();
        state.append_metadata_debts(self.entries);
        state.metadata_cleanup_in_progress = false;
    }
}

impl PreparedCleanup<'_, MetadataDebts> {
    fn execute(
        self,
        host: &crate::registration::RegistrationHost<'_>,
    ) -> crate::registration::MetadataDebtRetryResult {
        self.begin().execute(host)
    }
}

impl ExecutingCleanup<'_, MetadataDebts> {
    fn execute(
        self,
        host: &crate::registration::RegistrationHost<'_>,
    ) -> crate::registration::MetadataDebtRetryResult {
        let Self { ledger, entries } = self;
        let mut outcome =
            crate::registration::recovery::retry_metadata_debt_with_host(host, &entries);
        let mut state = ledger.state.lock();
        state.append_metadata_debts(std::mem::take(&mut outcome.remaining));
        state.metadata_cleanup_in_progress = false;
        outcome
    }
}

#[cfg(test)]
mod cleanup_tests {
    use super::*;
    use crate::registration::RegistrationId;

    fn ledger() -> HostLedger {
        let ledger = HostLedger::new();
        ledger.merge(HostMutationJournal::from_registrations([RegistrationId {
            id: 7.0,
            excel_name: "TEST.CLEANUP.CLAIM",
        }]));
        ledger
    }

    #[test]
    fn miri_unstarted_cleanup_claim_restores_its_obligations() {
        let ledger = ledger();
        let claim = ledger.claim_cleanup().unwrap();
        assert!(ledger.registrations_snapshot().is_empty());
        assert_eq!(ledger.registration_resources(), (0, false));
        assert!(!ledger.generation_callbacks_detached());
        assert!(!ledger.is_quiescent());
        assert!(ledger.claim_cleanup().is_err());
        drop(ledger.claim_metadata_cleanup().unwrap());
        assert!(ledger.claim_cleanup().is_err());
        drop(claim);
        assert_eq!(ledger.registrations_snapshot().len(), 1);
        assert_eq!(ledger.registration_resources(), (1, true));
        assert!(!ledger.is_quiescent());
        assert!(!ledger.registration_state_unknown());
    }

    #[test]
    #[allow(clippy::mem_forget, reason = "safe forgetting must block completion")]
    fn miri_forgotten_cleanup_claim_blocks_empty_ledger_certification() {
        let ledger = HostLedger::new();
        let claim = ledger.claim_cleanup().unwrap();
        std::mem::forget(claim);
        assert_eq!(ledger.registration_resources(), (0, false));
        assert!(ledger.registrations_snapshot().is_empty());
        assert!(!ledger.generation_callbacks_detached());
        assert!(!ledger.is_quiescent());
        assert!(ledger.registration_state_unknown());
        assert!(ledger.claim_cleanup().is_err());
    }

    #[test]
    fn miri_interrupted_cleanup_retains_uncertain_gate() {
        let ledger = ledger();
        let claim = ledger.claim_cleanup().unwrap();
        let outcome =
            crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(move || {
                let _executing = claim.begin();
                panic!("interrupted after cleanup authority transfer");
            }));
        assert!(outcome.is_err());
        assert!(ledger.registrations_snapshot().is_empty());
        assert_eq!(ledger.registration_resources(), (0, false));
        assert!(!ledger.generation_callbacks_detached());
        assert!(!ledger.is_quiescent());
        assert!(ledger.registration_state_unknown());
        assert!(matches!(
            ledger.claim_cleanup(),
            Err(crate::XllError::Closing)
        ));
    }

    #[test]
    fn miri_cleanup_claim_advances_only_after_actual_host_success() {
        use crate::host_callback::HostCallbackSession;
        use crate::registration::host::test_support::{CallbackScript, Reply};
        use xlfn_sys::{XLF_EVALUATE, XLF_SET_NAME, XLF_UNREGISTER, XLOPER12};
        let ledger = ledger();
        {
            let script =
                CallbackScript::install([Reply::success(XLF_UNREGISTER, XLOPER12::boolean(false))]);
            let callbacks = HostCallbackSession::new();
            let report = ledger
                .cleanup_registrations(&crate::registration::RegistrationHost::new(&callbacks))
                .unwrap();
            assert_eq!(report.failed.len(), 1);
            assert!(!ledger.generation_callbacks_detached());
            assert!(!ledger.is_quiescent());
            script.assert_calls(&[XLF_UNREGISTER]);
        }
        {
            let script = CallbackScript::install([
                Reply::success(XLF_UNREGISTER, XLOPER12::boolean(true)),
                Reply::success(XLF_EVALUATE, XLOPER12::number(7.0)),
                Reply::success(XLF_SET_NAME, XLOPER12::boolean(true)),
            ]);
            let callbacks = HostCallbackSession::new();
            let report = ledger
                .cleanup_registrations(&crate::registration::RegistrationHost::new(&callbacks))
                .unwrap();
            assert_eq!(report.succeeded.len(), 1);
            assert!(report.failed.is_empty());
            assert!(ledger.generation_callbacks_detached());
            assert!(ledger.is_quiescent());
            script.assert_calls(&[XLF_UNREGISTER, XLF_EVALUATE, XLF_SET_NAME]);
        }
    }

    #[test]
    fn miri_metadata_retry_preserves_debt_merged_during_host_callback() {
        use crate::host_callback::HostCallbackSession;
        use crate::registration::host::test_support::{CallbackScript, Reply};
        use xlfn_sys::{XLF_EVALUATE, XLOPER12};
        let ledger = std::sync::Arc::new(HostLedger::new());
        ledger.retain_metadata_debt(vec![MetadataDebt::new(
            RegistrationId {
                id: 7.0,
                excel_name: "OLD.DEBT",
            },
            crate::XllError::Closing,
        )]);
        let script =
            CallbackScript::install([Reply::success(XLF_EVALUATE, XLOPER12::number(99.0))]);
        let reenter = std::sync::Arc::clone(&ledger);
        script.on_call(XLF_EVALUATE, move || {
            assert!(!reenter.is_quiescent());
            assert!(reenter.has_metadata_debt());
            reenter.retain_metadata_debt(vec![MetadataDebt::new(
                RegistrationId {
                    id: 9.0,
                    excel_name: "NEW.DEBT",
                },
                crate::XllError::Closing,
            )]);
        });
        let callbacks = HostCallbackSession::new();
        let outcome =
            ledger.retry_metadata_cleanup(&crate::registration::RegistrationHost::new(&callbacks));
        assert!(outcome.terminal.is_none());
        let remaining = ledger.metadata_debt_snapshot();
        assert_eq!(remaining.len(), 2);
        assert!(remaining.contains_key(&ExcelNameKey::new("OLD.DEBT")));
        assert!(remaining.contains_key(&ExcelNameKey::new("NEW.DEBT")));
        script.assert_calls(&[XLF_EVALUATE]);
    }

    #[test]
    fn miri_unstarted_metadata_claim_restores_debt_and_releases_gate() {
        let ledger = HostLedger::new();
        ledger.retain_metadata_debt(vec![MetadataDebt::new(
            RegistrationId {
                id: 7.0,
                excel_name: "OLD.DEBT",
            },
            crate::XllError::Closing,
        )]);
        let claim = ledger.claim_metadata_cleanup().unwrap();
        assert!(ledger.metadata_debt_snapshot().is_empty());
        assert!(!ledger.is_quiescent());
        assert!(ledger.claim_metadata_cleanup().is_err());
        drop(ledger.claim_cleanup().unwrap());
        assert!(ledger.claim_metadata_cleanup().is_err());
        drop(claim);
        assert_eq!(ledger.metadata_debt_snapshot().len(), 1);
        assert!(
            ledger.is_quiescent(),
            "metadata debt alone remains best effort"
        );
    }

    #[test]
    fn miri_interrupted_metadata_cleanup_keeps_gate_and_concurrent_debt() {
        let ledger = HostLedger::new();
        ledger.retain_metadata_debt(vec![MetadataDebt::new(
            RegistrationId {
                id: 7.0,
                excel_name: "OLD.DEBT",
            },
            crate::XllError::Closing,
        )]);
        let claim = ledger.claim_metadata_cleanup().unwrap();
        ledger.retain_metadata_debt(vec![MetadataDebt::new(
            RegistrationId {
                id: 9.0,
                excel_name: "NEW.DEBT",
            },
            crate::XllError::Closing,
        )]);
        let outcome =
            crate::panic_boundary::catch_no_unwind(std::panic::AssertUnwindSafe(move || {
                let _executing = claim.begin();
                panic!("interrupted after metadata cleanup authority transfer");
            }));
        assert!(outcome.is_err());
        let remaining = ledger.metadata_debt_snapshot();
        assert_eq!(remaining.len(), 1);
        assert!(remaining.contains_key(&ExcelNameKey::new("NEW.DEBT")));
        assert!(ledger.has_metadata_debt());
        assert!(!ledger.generation_callbacks_detached());
        assert!(!ledger.is_quiescent());
        assert!(ledger.registration_state_unknown());
        assert!(matches!(
            ledger.claim_metadata_cleanup(),
            Err(crate::XllError::Closing),
        ));
    }
}
