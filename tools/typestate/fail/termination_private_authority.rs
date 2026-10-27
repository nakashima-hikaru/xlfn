// module: subscription
use super::host::SubscriptionHost;
use super::server::{CancelPending, ServerTermination};

fn invalid<H: SubscriptionHost>(pending: ServerTermination<'_, H, CancelPending>) {
    let _ = &pending.resources; // error: E0616
    let _ = &pending.state; // error: E0616
}
