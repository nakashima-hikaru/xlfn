// module: subscription
use super::server::CancelRequested;

fn invalid(requested: &CancelRequested) {
    let _ = &requested.error; // error: E0616
}
