// module: runtime_components
use super::HostLedger;
fn invalid(ledger: &HostLedger) {
    ledger.clear_metadata_debt_for_registrations(&[]); // error: E0599
    ledger.replace_metadata_debt(Default::default()); // error: E0599
}
