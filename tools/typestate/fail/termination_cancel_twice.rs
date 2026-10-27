// module: subscription
use super::host::SubscriptionHost;
use super::server::{CancelPending, ServerTermination};

fn invalid<H: SubscriptionHost>(pending: ServerTermination<'_, H, CancelPending>) {
    let requested = pending.request_cancel();
    let _ = pending.request_cancel(); // error: E0382
    drop(requested);
}
