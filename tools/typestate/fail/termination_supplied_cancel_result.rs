// module: subscription
use super::host::SubscriptionHost;
use super::server::{CancelRequested, ServerTermination};

fn invalid<H: SubscriptionHost>(
    requested: ServerTermination<'_, H, (), crate::DynamicRtdSources<()>, CancelRequested>,
) {
    let _ = requested.finish(Ok(())); // error: E0061
}
