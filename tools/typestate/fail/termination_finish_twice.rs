// module: subscription
use super::host::SubscriptionHost;
use super::server::{CancelRequested, ServerTermination};

fn invalid<H: SubscriptionHost>(requested: ServerTermination<'_, H, CancelRequested>) {
    let _ = requested.finish();
    let _ = requested.finish(); // error: E0382
}
