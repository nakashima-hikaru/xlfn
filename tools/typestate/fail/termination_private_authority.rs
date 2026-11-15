// module: subscription
use super::host::SubscriptionHost;
use super::server::{CancelPending, ServerTermination};

fn invalid<H: SubscriptionHost>(
    pending: ServerTermination<'_, H, (), crate::DynamicRtdSources<()>, CancelPending>,
) {
    let _ = &pending.pending; // error: E0616
}
