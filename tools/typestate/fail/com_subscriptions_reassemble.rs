// module: excel_rtd/windows
use super::server::AttachedSubscriptions;
use super::subscription_backend::{BackendSubscriptions, BackendSubscriptionServer};

fn invalid(runtime: BackendSubscriptions, server: BackendSubscriptionServer) {
    let _ = AttachedSubscriptions { runtime, server }; // error: E0451
}
