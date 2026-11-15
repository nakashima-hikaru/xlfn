// module: runtime/transactions
use super::{HostCleanupContext, RemovalClaimed};

fn drain_twice<A: crate::Addin>(claimed: RemovalClaimed<'_, '_, A>) {
    let _ = claimed.drain();
    let _ = claimed.drain(); // error: E0382
}

fn restart<A: crate::Addin>(host: HostCleanupContext<'_, '_, A>) {
    let _ = host.drain(); // error: E0599
}
