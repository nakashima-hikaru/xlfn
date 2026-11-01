// module: registration
use super::{HostRegistrar, RegistrationHost};
use super::ledger::CleanupSnapshot;
fn invalid(host: &RegistrationHost<'_>, observations: Vec<CleanupSnapshot>) {
    let _ = HostRegistrar::unregister_pending(host, observations); // error: E0308
}
