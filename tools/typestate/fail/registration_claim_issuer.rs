// module: runtime_components
use super::HostLedger;
fn invalid(ledger: &HostLedger) {
    let _ = ledger.claim_cleanup(); // error: E0624
}
