// module: runtime_components/host_ledger
use super::{PendingRegistration, PreparedCleanup};
use crate::registration::RegistrationHost;
fn invalid(claim: PreparedCleanup<'_, Vec<PendingRegistration>>, host: &RegistrationHost<'_>) {
    let _ = claim.execute(host);
    let _again = claim.execute(host); // error: E0382
}
