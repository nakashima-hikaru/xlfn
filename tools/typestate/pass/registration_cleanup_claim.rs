// module: runtime_components
use super::HostLedger;
use crate::registration::{HostMutationJournal, RegistrationHost, RegistrationId};
fn ordered(ledger: &HostLedger, host: &RegistrationHost<'_>, registration: RegistrationId) -> crate::XllResult<()> {
    ledger.merge(HostMutationJournal::from_registrations([registration]));
    let _quiescent = ledger.is_quiescent();
    let _report = ledger.cleanup_registrations(host)?;
    Ok(())
}
