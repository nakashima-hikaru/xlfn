// module: runtime_components/host_ledger
use super::{MetadataDebts, PreparedCleanup};
use crate::registration::RegistrationHost;

fn invalid(claim: PreparedCleanup<'_, MetadataDebts>, host: &RegistrationHost<'_>) {
    let executing = claim.begin();
    let _ = claim.execute(host); // error: E0382
    let _ = executing.execute(host);
}
