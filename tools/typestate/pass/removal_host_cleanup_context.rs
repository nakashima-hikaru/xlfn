// module: runtime/transactions
use super::{HostCleanupContext, RemovalClaimed};

fn cleanup<A: crate::Addin>(claimed: RemovalClaimed<'_, '_, A>) {
    if let Ok((_teardown, HostCleanupContext { deps, callbacks })) = claimed.drain() {
        let host = crate::registration::RegistrationHost::new(callbacks);
        let _ = deps.host().cleanup_registrations(&host);
    }
}
