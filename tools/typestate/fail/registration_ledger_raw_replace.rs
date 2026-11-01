// module: runtime_components
use super::HostLedger;
fn invalid(ledger: &HostLedger) {
    ledger.replace_registrations(Vec::new()); // error: E0599
    let _ = &ledger.state; // error: E0616
}
