// module: subscription
use super::host::SubscriptionHost;
use super::server::{CancelPending, ServerTermination};

fn invalid<H: SubscriptionHost>(
    pending: ServerTermination<'_, H, (), crate::DynamicRtdSources<()>, CancelPending>,
) {
    let _ = pending.drain_readers(); // error: E0599
    let _ = pending.finish(); // error: E0599
}
