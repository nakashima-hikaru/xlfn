// module: runtime_components/host_ledger
use super::RegistrationCleanupClaim;
use crate::registration::RegistrationHost;
fn invalid(claim: RegistrationCleanupClaim<'_>, host: &RegistrationHost<'_>) {
    let _ = claim.execute(host);
    let _again = claim.execute(host); // error: E0382
}
